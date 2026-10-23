// SPDX-License-Identifier: GPL-3.0-or-later
pragma solidity ^0.8.19;

import {VotingEscrow} from "../windv2/contracts/VotingEscrow.sol";
import {IVotingEscrow} from "../windv2/contracts/interfaces/IVotingEscrow.sol";
import {IReward} from "../windv2/contracts/interfaces/IReward.sol";
import {ManagedRewardsFactory} from "../windv2/contracts/factories/ManagedRewardsFactory.sol";
import {MockVeWind} from "../src/ve/MockVeWind.sol";
import {VeFullMockVoter, VeFullMockFactoryRegistry} from "../src/ve/VeFullMocks.sol";

interface Vm {
    function warp(uint256 newTimestamp) external;
    function roll(uint256 newHeight) external;
    function prank(address msgSender) external;
    function addr(uint256 privateKey) external pure returns (address);
    function sign(uint256 privateKey, bytes32 digest) external pure returns (uint8 v, bytes32 r, bytes32 s);
    function writeFile(string calldata path, string calldata data) external;
    function writeLine(string calldata path, string calldata data) external;
    function toString(uint256 value) external pure returns (string memory);
    function toString(int256 value) external pure returns (string memory);
}

/// @notice Full differential generator for `VotingEscrow`: the core lock logic of `GenerateVotingEscrow.t.sol`
/// plus voting checkpoints and delegation (`delegate`, `delegateBySig`, `getPastVotes`), and managed veNFTs
/// (`createManagedLockFor`, `depositManaged`, `withdrawManaged`, `setManagedState`, `setAllowedManager`), with
/// their `LockedManagedReward` / `FreeManagedReward` contracts (real windv2 code, deployed by the real
/// `ManagedRewardsFactory`). Only the Voter and the FactoryRegistry are mocks (`src/ve/VeFullMocks.sol`).
///
/// Output: `vectors/voting_escrow_full.txt`, one line per call:
///
///   op|now|block|args|ok|out|G|T|M
///
/// - `init` is the first line: `init|now|block|wind|ok||G||` where `wind` is the escrow token address.
/// - `args`: comma-separated decimal values. Addresses are their decimal `uint160` value. Layouts:
///   `setVoterAndDistributor`: caller,voter,distributor. `setAllowedManager`: caller,manager.
///   `createManagedLockFor`: caller,to. `depositManaged`: voter,token,m. `withdrawManaged`: voter,token.
///   `setManagedState`: caller,m,state. `freeNotify`: caller,m,token,amount,whitelisted.
///   `freeClaim`: caller,m,token,tokens... `delegate`: caller,delegator,delegatee.
///   `delegateBySig`: signer,delegator,delegatee,nonce,expiry,sOk,recovered. `getPastVotes`: account,token,t.
///   Core ops keep their layouts from `GenerateVotingEscrow.t.sol`.
/// - `ok` is `ok` or `err`. `out` is the return value on success or the revert reason on `err`.
///   `createLock(For)` returns `token:pulled`, `split` returns `t1:t2`, `withdraw` the WIND paid out,
///   `withdrawManaged` `total:reward`, `createManagedLockFor` the managed id, `freeClaim` the amounts paid
///   per token (comma-separated), `delegateBySig` the signer's nonce after the call, `getPastVotes` the
///   votes. Pulled amounts are the caller's balance change.
/// - `G` is `epoch,supply,permanentLockBalance,bias,slope,ts,blk,permanentLockBalance` (global point).
/// - `T` lists the touched tokens, each as `id:owner,amount,end,isPermanent,userEpoch,bias,slope,ts,blk,
///   permanent,slopeChangePre,slopeChangePost,escrowType,idToManaged,weight,delegatee,numCheckpoints,cps,
///   dm,lmrBalance,lmrNumCp,lmrEarned,fmrBalance,fmrNumCp,fmrEarned`. `cps` is the voting checkpoint history
///   `fromTimestamp.owner.delegatedBalance.delegatee` joined by `~` (or `-`). `dm` is the managed id whose
///   reward contracts are read (the managed id the token was last deposited into, or 0), and `*Earned` is
///   `earned(WIND, token)` now.
/// - `M` lists the touched managed ids as `m:deactivated,lmrSupply,lmrSupplyNumCp,fmrSupply,fmrSupplyNumCp`.
/// - Revert reasons are the custom error names, `Panic17` for checked arithmetic, `SafeCast*`, or `unknown`.
///
/// Setup: `VotingEscrow(0, WIND, registry, TEAM)`; TEAM sets the voter mock and distributor, GOV sets the
/// allowed manager. Users are `vm.addr(pk)`, so they can sign `delegateBySig` digests.
///
/// Run: `forge test --match-test testGenerateVotingEscrowFull`
contract GenerateVotingEscrowFullVectors {
    Vm constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

    uint256 constant STEPS = 3000;
    uint256 constant T0 = 1_780_000_000;
    uint256 constant B0 = 1_000;
    uint256 constant WEEK = 1 weeks;
    uint256 constant YEAR = 365 days;
    uint256 constant MAXTIME = 4 * 365 * 86400;
    uint256 constant NUSERS = 5;
    uint256 constant SECP256K1_N = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141;
    uint256 constant S_MAX = 0x7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF5D576E7357A4501DDFE92F46681B20A0;
    address constant TEAM = address(uint160(777));
    address constant GOV = address(uint160(888));
    address constant EC = address(uint160(999));
    address constant DIST = address(uint160(4444));
    address constant MANAGER = address(uint160(6000));
    address constant OUTSIDER = address(uint160(5000));
    string constant F = "vectors/voting_escrow_full.txt";

    uint256 seed = 0x564f5446; // "VOTF"
    uint256 clock;
    uint256 blk;
    MockVeWind wind;
    MockVeWind wind2;
    MockVeWind wind3;
    ManagedRewardsFactory factory;
    VeFullMockFactoryRegistry registry;
    VotingEscrow ve;
    VeFullMockVoter voterMock;
    address[5] users;
    uint256[5] pks;
    // Managed ids in creation order, and the managed id each token was last deposited into (0 if none).
    uint256[] managedIds;
    uint256[] depositedTokens;
    mapping(uint256 => uint256) depositedM;
    // Pre-call lock ends of the ids a call touches, for `slopeChangePre`.
    uint256[5] snapIds;
    uint256[5] snapEnds;
    uint256 lastMoved;

    function testGenerateVotingEscrowFull() public {
        vm.writeFile(F, "");
        deploy();
        logInit();
        setVoterOp();
        setManagerOp();
        for (uint256 i = 0; i < STEPS; i++) {
            step();
        }
    }

    /*//////////////////////////////////////////////////////////////
                                 SETUP
    //////////////////////////////////////////////////////////////*/

    function deploy() internal {
        clock = T0;
        blk = B0;
        vm.warp(clock);
        vm.roll(blk);
        wind = new MockVeWind();
        wind2 = new MockVeWind();
        wind3 = new MockVeWind();
        factory = new ManagedRewardsFactory();
        registry = new VeFullMockFactoryRegistry(address(factory));
        ve = new VotingEscrow(address(0), address(wind), address(registry), TEAM);
        voterMock = new VeFullMockVoter(address(ve), GOV, EC);
        voterMock.setWhitelisted(address(wind), true);
        voterMock.setWhitelisted(address(wind2), true);
        // wind3 stays unwhitelisted: notifying it as a new free reward reverts `NotWhitelisted`.
        for (uint256 i = 0; i < NUSERS; i++) {
            pks[i] = 0xA11CE0 + i;
            users[i] = vm.addr(pks[i]);
            wind.testMint(users[i], 1 << 127);
            vm.prank(users[i]);
            wind.approve(address(ve), type(uint256).max);
        }
        wind.testMint(OUTSIDER, 1 << 127);
        vm.prank(OUTSIDER);
        wind.approve(address(ve), type(uint256).max);
        wind.testMint(DIST, 1 << 127);
        vm.prank(DIST);
        wind.approve(address(ve), type(uint256).max);
    }

    function logInit() internal {
        vm.writeLine(F, string.concat("init|", s(clock), "|", s(blk), "|", a2s(address(wind)), "|ok||", gstate(), "||"));
    }

    function setVoterOp() internal {
        string memory args = string.concat(a2s(TEAM), ",", a2s(address(voterMock)), ",", a2s(DIST));
        vm.prank(TEAM);
        try ve.setVoterAndDistributor(address(voterMock), DIST) {
            logLine("setVoterAndDistributor", args, true, "", 0, 0, 0);
        } catch (bytes memory r) {
            logLine("setVoterAndDistributor", args, false, reasonOf(r), 0, 0, 0);
        }
    }

    function setManagerOp() internal {
        vm.prank(GOV);
        try ve.setAllowedManager(MANAGER) {
            logLine("setAllowedManager", string.concat(a2s(GOV), ",", a2s(MANAGER)), true, "", 0, 0, 0);
        } catch (bytes memory r) {
            logLine("setAllowedManager", string.concat(a2s(GOV), ",", a2s(MANAGER)), false, reasonOf(r), 0, 0, 0);
        }
    }

    /*//////////////////////////////////////////////////////////////
                                 STEPS
    //////////////////////////////////////////////////////////////*/

    function step() internal {
        advance();
        uint256 p = rand() % 100;
        if (p < 8) doCreate(false);
        else if (p < 12) doCreate(true);
        else if (p < 18) doDeposit(false);
        else if (p < 24) doDeposit(true);
        else if (p < 28) doIncreaseUnlock();
        else if (p < 33) doWithdraw();
        else if (p < 38) doMerge();
        else if (p < 43) doSplit();
        else if (p < 45) doToggleRandom();
        else if (p < 49) doPermanent(true);
        else if (p < 52) doPermanent(false);
        else if (p < 56) doTransfer();
        else if (p < 58) doVoting();
        else if (p < 60) doCheckpoint();
        else if (p < 62) doReadBalance();
        else if (p < 64) doReadBalanceAt();
        else if (p < 65) doReadSupply();
        else if (p < 66) doReadSupplyAt();
        else if (p < 70) doDelegate();
        else if (p < 73) doDelegateBySig();
        else if (p < 76) doGetPastVotes();
        else if (p < 79) doCreateManaged();
        else if (p < 84) doDepositManaged();
        else if (p < 88) doWithdrawManaged();
        else if (p < 89) doSetManagedState();
        else if (p < 90) doSetAllowedManager();
        else if (p < 93) doFreeNotify();
        else if (p < 96) doFreeClaim();
        else if (p < 99) doFeedManaged(rand() % 2 == 0);
        else doWithdrawManaged();
        require(conservation(), "conservation");
    }

    /// Escrow WIND plus the WIND held by every locked reward contract is the total supply.
    function conservation() internal view returns (bool) {
        uint256 held = wind.balanceOf(address(ve));
        for (uint256 i = 0; i < managedIds.length; i++) {
            held += wind.balanceOf(ve.managedToLocked(managedIds[i]));
        }
        return held == ve.supply();
    }

    /// Time moves forward: sometimes exactly to a week boundary, mostly by less than two weeks, rarely by
    /// 5 to 6 years (so the 255-week cap in the global loop is reached and every lock expires).
    function advance() internal {
        uint256 p = rand() % 1000;
        uint256 d;
        if (p < 300) d = WEEK - (clock % WEEK);
        else if (p < 850) d = 1 + rand() % (WEEK / 4);
        else if (p < 999) d = 1 + rand() % (2 * WEEK);
        else d = 5 * YEAR + rand() % YEAR;
        clock += d;
        vm.warp(clock);
        blk += rand() % 3;
        vm.roll(blk);
    }

    /*//////////////////////////////////////////////////////////////
                                  CORE OPS
    //////////////////////////////////////////////////////////////*/

    function doCreate(bool isFor) internal {
        address caller = pickActor(0);
        address to = isFor ? users[rand() % NUSERS] : caller;
        uint256 value = randValue();
        uint256 dur = randDuration();
        ensureFunds(caller, value);
        snap3(0, 0, 0);
        uint256 before = wind.balanceOf(caller);
        if (isFor) {
            string memory args = string.concat(a2s(caller), ",", a2s(to), ",", s(value), ",", s(dur));
            vm.prank(caller);
            try ve.createLockFor(value, dur, to) returns (uint256 id) {
                logLine("createLockFor", args, true, string.concat(s(id), ":", s(before - wind.balanceOf(caller))), id, 0, 0);
            } catch (bytes memory r) {
                logLine("createLockFor", args, false, reasonOf(r), 0, 0, 0);
            }
        } else {
            string memory args2 = string.concat(a2s(caller), ",", s(value), ",", s(dur));
            vm.prank(caller);
            try ve.createLock(value, dur) returns (uint256 id) {
                logLine("createLock", args2, true, string.concat(s(id), ":", s(before - wind.balanceOf(caller))), id, 0, 0);
            } catch (bytes memory r) {
                logLine("createLock", args2, false, reasonOf(r), 0, 0, 0);
            }
        }
    }

    function doDeposit(bool increase) internal {
        uint256 id = pickToken();
        address caller = pickActor(id);
        uint256 value = randValue();
        ensureFunds(caller, value);
        snap3(id, 0, 0);
        uint256 before = wind.balanceOf(caller);
        string memory args = string.concat(a2s(caller), ",", s(id), ",", s(value));
        vm.prank(caller);
        if (increase) {
            try ve.increaseAmount(id, value) {
                logLine("increaseAmount", args, true, s(before - wind.balanceOf(caller)), id, 0, 0);
            } catch (bytes memory r) {
                logLine("increaseAmount", args, false, reasonOf(r), id, 0, 0);
            }
        } else {
            try ve.depositFor(id, value) {
                logLine("depositFor", args, true, s(before - wind.balanceOf(caller)), id, 0, 0);
            } catch (bytes memory r) {
                logLine("depositFor", args, false, reasonOf(r), id, 0, 0);
            }
        }
    }

    function doIncreaseUnlock() internal {
        uint256 id = pickToken();
        address caller = pickActor(id);
        uint256 dur = randDuration();
        snap3(id, 0, 0);
        string memory args = string.concat(a2s(caller), ",", s(id), ",", s(dur));
        vm.prank(caller);
        try ve.increaseUnlockTime(id, dur) {
            logLine("increaseUnlockTime", args, true, "", id, 0, 0);
        } catch (bytes memory r) {
            logLine("increaseUnlockTime", args, false, reasonOf(r), id, 0, 0);
        }
    }

    function doWithdraw() internal {
        uint256 id = pickToken();
        address caller = pickActor(id);
        snap3(id, 0, 0);
        string memory args = string.concat(a2s(caller), ",", s(id));
        uint256 before = wind.balanceOf(caller);
        vm.prank(caller);
        try ve.withdraw(id) {
            logLine("withdraw", args, true, s(wind.balanceOf(caller) - before), id, 0, 0);
        } catch (bytes memory r) {
            logLine("withdraw", args, false, reasonOf(r), id, 0, 0);
        }
    }

    function doMerge() internal {
        uint256 f = pickToken();
        address caller = pickActor(f);
        uint256 t;
        uint256 r = rand() % 4;
        if (r == 0) t = findOwnedPermanent(caller, f);
        else if (r == 1) t = findOwned(caller, f);
        else t = pickToken();
        if (t == 0) t = pickToken();
        if (rand() % 25 == 0) t = f;
        snap3(f, t, 0);
        string memory args = string.concat(a2s(caller), ",", s(f), ",", s(t));
        vm.prank(caller);
        try ve.merge(f, t) {
            logLine("merge", args, true, "", f, t, 0);
        } catch (bytes memory r2) {
            logLine("merge", args, false, reasonOf(r2), f, t, 0);
        }
    }

    function doSplit() internal {
        uint256 f = rand() % 4 == 0 ? findPermanent() : pickToken();
        if (f == 0) f = pickToken();
        address caller = pickActor(f);
        uint256 amt = randSplitAmount(f);
        snap3(f, 0, 0);
        string memory args = string.concat(a2s(caller), ",", s(f), ",", s(amt));
        vm.prank(caller);
        try ve.split(f, amt) returns (uint256 t1, uint256 t2) {
            logLine("split", args, true, string.concat(s(t1), ":", s(t2)), f, t1, t2);
        } catch (bytes memory r) {
            logLine("split", args, false, reasonOf(r), f, 0, 0);
        }
    }

    function doToggleRandom() internal {
        address caller = rand() % 10 == 0 ? users[rand() % NUSERS] : TEAM;
        address account = rand() % 4 == 0 ? address(0) : users[rand() % NUSERS];
        toggle(caller, account, rand() % 4 != 0);
    }

    function toggle(address caller, address account, bool flag) internal {
        string memory args = string.concat(a2s(caller), ",", a2s(account), ",", b01(flag));
        vm.prank(caller);
        try ve.toggleSplit(account, flag) {
            logLine("toggleSplit", args, true, "", 0, 0, 0);
        } catch (bytes memory r) {
            logLine("toggleSplit", args, false, reasonOf(r), 0, 0, 0);
        }
    }

    function doPermanent(bool lock_) internal {
        if (lock_) {
            uint256 id = pickToken();
            address caller = pickActor(id);
            snap3(id, 0, 0);
            string memory args = string.concat(a2s(caller), ",", s(id));
            vm.prank(caller);
            try ve.lockPermanent(id) {
                logLine("lockPermanent", args, true, "", id, 0, 0);
            } catch (bytes memory r) {
                logLine("lockPermanent", args, false, reasonOf(r), id, 0, 0);
            }
        } else if (rand() % 4 == 0) {
            // Transfer a permanent token to its owner, then unlock it in the same block: `OwnershipChange`.
            uint256 pid = findPermanent();
            if (pid != 0) {
                address owner = ve.ownerOf(pid);
                snap3(pid, 0, 0);
                string memory targs = string.concat(a2s(owner), ",", a2s(owner), ",", a2s(owner), ",", s(pid));
                vm.prank(owner);
                try ve.transferFrom(owner, owner, pid) {
                    lastMoved = pid;
                    logLine("transferFrom", targs, true, "", pid, 0, 0);
                } catch (bytes memory r) {
                    logLine("transferFrom", targs, false, reasonOf(r), pid, 0, 0);
                }
                unlockOp(owner, pid);
            }
        } else {
            uint256 pm = findPermanent();
            if (pm != 0 && rand() % 2 == 0) {
                unlockOp(ve.ownerOf(pm), pm);
            } else {
                uint256 id = pickMoved();
                unlockOp(pickActor(id), id);
            }
        }
    }

    function unlockOp(address caller, uint256 id) internal {
        snap3(id, 0, 0);
        string memory args = string.concat(a2s(caller), ",", s(id));
        vm.prank(caller);
        try ve.unlockPermanent(id) {
            logLine("unlockPermanent", args, true, "", id, 0, 0);
        } catch (bytes memory r) {
            logLine("unlockPermanent", args, false, reasonOf(r), id, 0, 0);
        }
    }

    function doTransfer() internal {
        uint256 id = pickToken();
        address owner = id == 0 ? address(0) : ve.ownerOf(id);
        address from = rand() % 5 == 0 ? users[rand() % NUSERS] : owner;
        address caller = rand() % 10 == 0 ? users[rand() % NUSERS] : owner;
        if (caller == address(0)) caller = OUTSIDER;
        address to = users[rand() % NUSERS];
        snap3(id, 0, 0);
        string memory args = string.concat(a2s(caller), ",", a2s(from), ",", a2s(to), ",", s(id));
        vm.prank(caller);
        try ve.transferFrom(from, to, id) {
            lastMoved = id;
            logLine("transferFrom", args, true, "", id, 0, 0);
        } catch (bytes memory r) {
            logLine("transferFrom", args, false, reasonOf(r), id, 0, 0);
        }
    }

    function doVoting() internal {
        uint256 id = pickToken();
        address caller = rand() % 10 == 0 ? users[rand() % NUSERS] : address(voterMock);
        bool flag = rand() % 2 == 0;
        snap3(id, 0, 0);
        string memory args = string.concat(a2s(caller), ",", s(id), ",", b01(flag));
        vm.prank(caller);
        try ve.voting(id, flag) {
            logLine("voting", args, true, "", id, 0, 0);
        } catch (bytes memory r) {
            logLine("voting", args, false, reasonOf(r), id, 0, 0);
        }
    }

    function doCheckpoint() internal {
        try ve.checkpoint() {
            logLine("checkpoint", "", true, "", 0, 0, 0);
        } catch (bytes memory r) {
            logLine("checkpoint", "", false, reasonOf(r), 0, 0, 0);
        }
    }

    function doReadBalance() internal {
        uint256 id = pickMoved();
        try ve.balanceOfNFT(id) returns (uint256 v) {
            logLine("balanceOfNFT", s(id), true, s(v), 0, 0, 0);
        } catch (bytes memory r) {
            logLine("balanceOfNFT", s(id), false, reasonOf(r), 0, 0, 0);
        }
    }

    function doReadBalanceAt() internal {
        uint256 id = pickToken();
        uint256 t = randPast();
        string memory args = string.concat(s(id), ",", s(t));
        try ve.balanceOfNFTAt(id, t) returns (uint256 v) {
            logLine("balanceOfNFTAt", args, true, s(v), 0, 0, 0);
        } catch (bytes memory r) {
            logLine("balanceOfNFTAt", args, false, reasonOf(r), 0, 0, 0);
        }
    }

    function doReadSupply() internal {
        try ve.totalSupply() returns (uint256 v) {
            logLine("totalSupply", "", true, s(v), 0, 0, 0);
        } catch (bytes memory r) {
            logLine("totalSupply", "", false, reasonOf(r), 0, 0, 0);
        }
    }

    function doReadSupplyAt() internal {
        uint256 t = randPast();
        try ve.totalSupplyAt(t) returns (uint256 v) {
            logLine("totalSupplyAt", s(t), true, s(v), 0, 0, 0);
        } catch (bytes memory r) {
            logLine("totalSupplyAt", s(t), false, reasonOf(r), 0, 0, 0);
        }
    }

    /*//////////////////////////////////////////////////////////////
                              DELEGATION OPS
    //////////////////////////////////////////////////////////////*/

    /// A permanent lock 50% of the time (only those can delegate), otherwise any token.
    function pickDelegator() internal returns (uint256 d) {
        if (rand() % 2 == 0) d = findPermanent();
        if (d == 0) d = pickToken();
    }

    function doDelegate() internal {
        uint256 d = pickDelegator();
        uint256 to;
        uint256 p = rand() % 20;
        if (p < 5) to = 0;
        else if (p < 6) to = d;
        else to = pickToken();
        address caller = pickActor(d);
        snap3(d, to, 0);
        string memory args = string.concat(a2s(caller), ",", s(d), ",", s(to));
        vm.prank(caller);
        try ve.delegate(d, to) {
            logLine("delegate", args, true, "", d, to, 0);
        } catch (bytes memory r) {
            logLine("delegate", args, false, reasonOf(r), d, to, 0);
        }
    }

    /// `delegateBySig` with a digest signed by a user key. Corruptions: an invalid `v` (recovers zero), a high
    /// `s` (`InvalidSignatureS`), a wrong nonce, an expired expiry, or a signer who does not own the token.
    function doDelegateBySig() internal {
        uint256 d = pickDelegator();
        uint256 to = rand() % 5 == 0 ? 0 : pickToken();
        uint256 idx = signerIndex(d);
        uint256 nonce = ve.nonces(users[idx]);
        if (rand() % 6 == 0) nonce += 1;
        uint256 expiry = rand() % 10 == 0 ? clock - 1 : clock + (rand() % (2 * WEEK));
        if (rand() % 25 == 0) expiry = type(uint256).max;
        snap3(d, to, 0);
        (uint8 v, bytes32 r, bytes32 sig, address recovered) = signDelegation(idx, d, to, nonce, expiry);
        bool sOk = uint256(sig) <= S_MAX;
        string memory args = delegateSigArgs(users[idx], d, to, nonce, expiry, sOk, recovered);
        callDelegateBySig(d, to, nonce, expiry, v, r, sig, recovered, args);
    }

    function callDelegateBySig(
        uint256 d,
        uint256 to,
        uint256 nonce,
        uint256 expiry,
        uint8 v,
        bytes32 r,
        bytes32 sig,
        address recovered,
        string memory args
    ) internal {
        try ve.delegateBySig(d, to, nonce, expiry, v, r, sig) {
            logLine("delegateBySig", args, true, s(ve.nonces(recovered)), d, to, 0);
        } catch (bytes memory e) {
            logLine("delegateBySig", args, false, reasonOf(e), d, to, 0);
        }
    }

    /// The signer: the owner of `d` 75% of the time when it is a user, otherwise a random user.
    function signerIndex(uint256 d) internal returns (uint256 idx) {
        idx = rand() % NUSERS;
        if (rand() % 4 == 0) return idx;
        address owner = ve.ownerOf(d);
        for (uint256 i = 0; i < NUSERS; i++) {
            if (users[i] == owner) idx = i;
        }
    }

    /// Signs the EIP-712 `Delegation` digest with the user key, then applies the corruptions: an invalid `v`
    /// (recovers zero), or a high `s` with flipped `v` (`InvalidSignatureS`). Returns the (possibly corrupted)
    /// signature and the address `ecrecover` gives for it.
    function signDelegation(uint256 idx, uint256 d, uint256 to, uint256 nonce, uint256 expiry)
        internal
        returns (uint8 v, bytes32 r, bytes32 sig, address recovered)
    {
        bytes32 domain = keccak256(
            abi.encode(
                ve.DOMAIN_TYPEHASH(),
                keccak256(bytes(ve.name())),
                keccak256(bytes(ve.version())),
                block.chainid,
                address(ve)
            )
        );
        bytes32 structHash = keccak256(abi.encode(ve.DELEGATION_TYPEHASH(), d, to, nonce, expiry));
        bytes32 digest = keccak256(abi.encodePacked("\x19\x01", domain, structHash));
        (v, r, sig) = vm.sign(pks[idx], digest);
        uint256 p = rand() % 100;
        if (p < 5) {
            v = 29;
        } else if (p < 10) {
            sig = bytes32(SECP256K1_N - uint256(sig));
            v = v == 27 ? 28 : 27;
        }
        recovered = ecrecover(digest, v, r, sig);
    }

    function delegateSigArgs(
        address signer,
        uint256 d,
        uint256 to,
        uint256 nonce,
        uint256 expiry,
        bool sOk,
        address recovered
    ) internal pure returns (string memory) {
        return string.concat(
            a2s(signer), ",", s(d), ",", s(to), ",", s(nonce), ",", s(expiry), ",", b01(sOk), ",", a2s(recovered)
        );
    }

    function doGetPastVotes() internal {
        // Permanent locks half the time: they can receive delegations, which is the delegated-balance branch.
        uint256 t = rand() % 4 != 0 ? findPermanentActive() : 0;
        if (t == 0) t = pickToken();
        uint256 p = rand() % 10;
        address account = p < 6 ? ve.ownerOf(t) : p < 7 ? address(0) : p < 8 ? OUTSIDER : users[rand() % NUSERS];
        uint256 at = rand() % 2 == 0 ? clock : randPast();
        snap3(t, 0, 0);
        string memory args = string.concat(a2s(account), ",", s(t), ",", s(at));
        try ve.getPastVotes(account, t, at) returns (uint256 v) {
            logLine("getPastVotes", args, true, s(v), t, 0, 0);
        } catch (bytes memory r) {
            logLine("getPastVotes", args, false, reasonOf(r), t, 0, 0);
        }
    }

    /*//////////////////////////////////////////////////////////////
                              MANAGED OPS
    //////////////////////////////////////////////////////////////*/

    function doCreateManaged() internal {
        uint256 p = rand() % 10;
        address caller = p < 3 ? GOV : p < 9 ? MANAGER : users[rand() % NUSERS];
        address to = rand() % 10 == 0 ? OUTSIDER : users[rand() % NUSERS];
        snap3(0, 0, 0);
        string memory args = string.concat(a2s(caller), ",", a2s(to));
        vm.prank(caller);
        try ve.createManagedLockFor(to) returns (uint256 m) {
            managedIds.push(m);
            logLine("createManagedLockFor", args, true, s(m), m, 0, 0);
        } catch (bytes memory r) {
            logLine("createManagedLockFor", args, false, reasonOf(r), 0, 0, 0);
        }
    }

    function doDepositManaged() internal {
        // A live normal lock most of the time, so the deposit usually succeeds.
        uint256 t = rand() % 10 < 8 ? findActiveNormal() : 0;
        if (t == 0) t = pickToken();
        uint256 m = pickManaged();
        snap3(t, m, 0);
        string memory args = string.concat(a2s(address(voterMock)), ",", s(t), ",", s(m));
        vm.prank(address(voterMock));
        try ve.depositManaged(t, m) {
            if (depositedM[t] == 0) depositedTokens.push(t);
            depositedM[t] = m;
            logLine("depositManaged", args, true, "", t, m, 0);
        } catch (bytes memory r) {
            logLine("depositManaged", args, false, reasonOf(r), t, m, 0);
        }
    }

    function doWithdrawManaged() internal {
        // A locked token with accrued locked reward half the time, so the reward branch is exercised.
        uint256 t = rand() % 2 == 0 ? findLockedWithReward() : 0;
        if (t == 0) t = findLocked();
        if (t == 0) t = pickToken();
        uint256 m = ve.idToManaged(t);
        snap3(t, m, 0);
        uint256 weight = ve.weights(t, m);
        uint256 reward = m == 0 ? 0 : IReward(ve.managedToLocked(m)).earned(address(wind), t);
        string memory args = string.concat(a2s(address(voterMock)), ",", s(t));
        vm.prank(address(voterMock));
        try ve.withdrawManaged(t) {
            logLine("withdrawManaged", args, true, string.concat(s(weight + reward), ":", s(reward)), t, m, 0);
        } catch (bytes memory r) {
            logLine("withdrawManaged", args, false, reasonOf(r), t, m, 0);
        }
    }

    function doSetManagedState() internal {
        uint256 m = pickManaged();
        bool st = rand() % 2 == 0;
        uint256 p = rand() % 10;
        address caller = p < 3 ? GOV : p < 9 ? EC : users[0];
        snap3(m, 0, 0);
        string memory args = string.concat(a2s(caller), ",", s(m), ",", b01(st));
        vm.prank(caller);
        try ve.setManagedState(m, st) {
            logLine("setManagedState", args, true, "", m, 0, 0);
        } catch (bytes memory r) {
            logLine("setManagedState", args, false, reasonOf(r), m, 0, 0);
        }
    }

    function doSetAllowedManager() internal {
        address caller = rand() % 6 == 0 ? users[0] : GOV;
        uint256 r = rand() % 4;
        address mgr = r == 0 ? MANAGER : r == 1 ? OUTSIDER : r == 2 ? address(0) : users[1];
        snap3(0, 0, 0);
        string memory args = string.concat(a2s(caller), ",", a2s(mgr));
        vm.prank(caller);
        try ve.setAllowedManager(mgr) {
            logLine("setAllowedManager", args, true, "", 0, 0, 0);
        } catch (bytes memory e) {
            logLine("setAllowedManager", args, false, reasonOf(e), 0, 0, 0);
        }
    }

    /// `depositFor` (managed: distributor only) or `increaseAmount` (owner only) on a managed veNFT. The
    /// amount is also a locked reward.
    function doFeedManaged(bool increase) internal {
        if (managedIds.length == 0) return;
        // A managed veNFT that has deposits 70% of the time, so the locked reward accrues to someone.
        uint256 m = depositedTokens.length > 0 && rand() % 10 < 7
            ? depositedM[depositedTokens[rand() % depositedTokens.length]]
            : pickManaged();
        address caller = rand() % 3 != 0 ? DIST : ve.ownerOf(m);
        if (caller == address(0)) caller = DIST;
        uint256 value = randValue();
        ensureFunds(caller, value);
        snap3(m, 0, 0);
        uint256 before = wind.balanceOf(caller);
        string memory args = string.concat(a2s(caller), ",", s(m), ",", s(value));
        vm.prank(caller);
        if (increase) {
            try ve.increaseAmount(m, value) {
                logLine("increaseAmount", args, true, s(before - wind.balanceOf(caller)), m, 0, 0);
            } catch (bytes memory r) {
                logLine("increaseAmount", args, false, reasonOf(r), m, 0, 0);
            }
        } else {
            try ve.depositFor(m, value) {
                logLine("depositFor", args, true, s(before - wind.balanceOf(caller)), m, 0, 0);
            } catch (bytes memory r) {
                logLine("depositFor", args, false, reasonOf(r), m, 0, 0);
            }
        }
    }

    /// `FreeManagedReward.notifyRewardAmount` on a managed veNFT's free reward contract.
    function doFreeNotify() internal {
        if (managedIds.length == 0) return;
        uint256 m = pickStrictManaged();
        address token = tokenAt(rand() % 3);
        uint256 amt = rand() % 10 == 0 ? 0 : randRewardAmount();
        address caller = rand() % 8 == 0 ? OUTSIDER : users[rand() % NUSERS];
        address fmr = ve.managedToFree(m);
        ensureTok(token, caller, amt);
        vm.prank(caller);
        MockVeWind(token).approve(fmr, amt);
        uint256 wl = voterMock.isWhitelistedToken(token) ? 1 : 0;
        snap3(m, 0, 0);
        string memory args = string.concat(a2s(caller), ",", s(m), ",", a2s(token), ",", s(amt), ",", s(wl));
        vm.prank(caller);
        try IReward(fmr).notifyRewardAmount(token, amt) {
            logLine("freeNotify", args, true, "", m, 0, 0);
        } catch (bytes memory r) {
            logLine("freeNotify", args, false, reasonOf(r), m, 0, 0);
        }
    }

    /// `FreeManagedReward.getReward(t, tokens)` as the owner (or a stranger, which reverts). Pays the owner.
    function doFreeClaim() internal {
        if (managedIds.length == 0) return;
        uint256 m = pickStrictManaged();
        uint256 t = depositedTokens.length > 0 && rand() % 10 < 8
            ? depositedTokens[rand() % depositedTokens.length]
            : pickToken();
        address owner = ve.ownerOf(t);
        address caller = rand() % 6 == 0 || owner == address(0) ? users[rand() % NUSERS] : owner;
        address[] memory tks = pickClaimTokens();
        uint256[] memory before = balancesOf(tks, owner);
        snap3(t, m, 0);
        callFreeClaim(caller, m, t, tks, owner, before, claimArgs(caller, m, t, tks));
    }

    /// One or two distinct reward tokens.
    function pickClaimTokens() internal returns (address[] memory tks) {
        uint256 i0 = rand() % 3;
        uint256 nt = 1 + rand() % 2;
        tks = new address[](nt);
        tks[0] = tokenAt(i0);
        if (nt == 2) tks[1] = tokenAt((i0 + 1 + rand() % 2) % 3);
    }

    function balancesOf(address[] memory tks, address who) internal view returns (uint256[] memory bal) {
        bal = new uint256[](tks.length);
        for (uint256 k = 0; k < tks.length; k++) {
            bal[k] = MockVeWind(tks[k]).balanceOf(who);
        }
    }

    function claimArgs(address caller, uint256 m, uint256 t, address[] memory tks)
        internal
        pure
        returns (string memory args)
    {
        args = string.concat(a2s(caller), ",", s(m), ",", s(t));
        for (uint256 k = 0; k < tks.length; k++) {
            args = string.concat(args, ",", a2s(tks[k]));
        }
    }

    function callFreeClaim(
        address caller,
        uint256 m,
        uint256 t,
        address[] memory tks,
        address owner,
        uint256[] memory before,
        string memory args
    ) internal {
        address fmr = ve.managedToFree(m);
        vm.prank(caller);
        try IReward(fmr).getReward(t, tks) {
            logLine("freeClaim", args, true, paidList(tks, owner, before), t, m, 0);
        } catch (bytes memory r) {
            logLine("freeClaim", args, false, reasonOf(r), t, m, 0);
        }
    }

    function paidList(address[] memory tks, address owner, uint256[] memory before)
        internal
        view
        returns (string memory out)
    {
        for (uint256 k = 0; k < tks.length; k++) {
            string memory piece = s(MockVeWind(tks[k]).balanceOf(owner) - before[k]);
            out = k == 0 ? piece : string.concat(out, ",", piece);
        }
    }

    /*//////////////////////////////////////////////////////////////
                               PICKERS
    //////////////////////////////////////////////////////////////*/

    function pickActor(uint256 id) internal returns (address) {
        uint256 r = rand() % 100;
        if (r < 70 && id != 0) {
            address o = ve.ownerOf(id);
            if (o != address(0)) return o;
        }
        if (r < 97) return users[rand() % NUSERS];
        return OUTSIDER;
    }

    function pickToken() internal returns (uint256) {
        uint256 n = ve.tokenId();
        uint256 r = rand() % 100;
        if (r == 3) return 0;
        if (n == 0 || r < 3) return n + 1 + (rand() % 2);
        return 1 + (rand() % n);
    }

    /// A managed veNFT 80% of the time (when there is one), otherwise any token.
    function pickManaged() internal returns (uint256) {
        if (managedIds.length == 0 || rand() % 5 == 0) return pickToken();
        return managedIds[rand() % managedIds.length];
    }

    /// Always a managed veNFT (the free reward ops need its reward contracts). Requires `managedIds` to be set.
    function pickStrictManaged() internal returns (uint256) {
        return managedIds[rand() % managedIds.length];
    }

    function pickMoved() internal returns (uint256) {
        if (lastMoved != 0 && rand() % 100 < 30) return lastMoved;
        return pickToken();
    }

    function findNormal() internal returns (uint256) {
        uint256 n = ve.tokenId();
        if (n == 0) return 0;
        uint256 start = rand() % n;
        for (uint256 k = 0; k < n; k++) {
            uint256 id = 1 + ((start + k) % n);
            if (uint256(ve.escrowType(id)) == 0 && ve.ownerOf(id) != address(0)) return id;
        }
        return 0;
    }

    /// Some token currently locked into a managed veNFT, or 0.
    function findLocked() internal returns (uint256) {
        uint256 n = ve.tokenId();
        if (n == 0) return 0;
        uint256 start = rand() % n;
        for (uint256 k = 0; k < n; k++) {
            uint256 id = 1 + ((start + k) % n);
            if (uint256(ve.escrowType(id)) == 1) return id;
        }
        return 0;
    }

    /// A normal veNFT with a positive, unexpired lock, or 0.
    function findActiveNormal() internal returns (uint256) {
        uint256 n = ve.tokenId();
        if (n == 0) return 0;
        uint256 start = rand() % n;
        for (uint256 k = 0; k < n; k++) {
            uint256 id = 1 + ((start + k) % n);
            IVotingEscrow.LockedBalance memory l = ve.locked(id);
            bool live = l.isPermanent || l.end > clock;
            if (uint256(ve.escrowType(id)) == 0 && l.amount > 0 && live && ve.ownerOf(id) != address(0)) return id;
        }
        return 0;
    }

    /// A locked token whose locked reward (WIND) is positive now, or 0.
    function findLockedWithReward() internal returns (uint256) {
        uint256 n = ve.tokenId();
        if (n == 0) return 0;
        uint256 start = rand() % n;
        for (uint256 k = 0; k < n; k++) {
            uint256 id = 1 + ((start + k) % n);
            if (uint256(ve.escrowType(id)) != 1) continue;
            uint256 m = ve.idToManaged(id);
            if (m != 0 && IReward(ve.managedToLocked(m)).earned(address(wind), id) > 0) return id;
        }
        return 0;
    }

    function findPermanent() internal returns (uint256) {
        uint256 n = ve.tokenId();
        if (n == 0) return 0;
        uint256 start = rand() % n;
        for (uint256 k = 0; k < n; k++) {
            uint256 id = 1 + ((start + k) % n);
            if (ve.locked(id).isPermanent && ve.ownerOf(id) != address(0)) return id;
        }
        return 0;
    }

    /// A permanent lock with a positive amount and an owner, or 0.
    function findPermanentActive() internal returns (uint256) {
        uint256 n = ve.tokenId();
        if (n == 0) return 0;
        uint256 start = rand() % n;
        for (uint256 k = 0; k < n; k++) {
            uint256 id = 1 + ((start + k) % n);
            IVotingEscrow.LockedBalance memory l = ve.locked(id);
            if (l.isPermanent && l.amount > 0 && ve.ownerOf(id) != address(0)) return id;
        }
        return 0;
    }

    function findOwned(address owner, uint256 avoid) internal returns (uint256) {
        uint256 n = ve.tokenId();
        if (n == 0) return 0;
        uint256 start = rand() % n;
        for (uint256 k = 0; k < n; k++) {
            uint256 id = 1 + ((start + k) % n);
            if (id != avoid && ve.ownerOf(id) == owner) return id;
        }
        return 0;
    }

    function findOwnedPermanent(address owner, uint256 avoid) internal returns (uint256) {
        uint256 n = ve.tokenId();
        if (n == 0) return 0;
        uint256 start = rand() % n;
        for (uint256 k = 0; k < n; k++) {
            uint256 id = 1 + ((start + k) % n);
            if (id != avoid && ve.ownerOf(id) == owner && ve.locked(id).isPermanent) return id;
        }
        return 0;
    }

    function tokenAt(uint256 i) internal view returns (address) {
        if (i == 0) return address(wind);
        if (i == 1) return address(wind2);
        return address(wind3);
    }

    function randValue() internal returns (uint256) {
        uint256 p = rand() % 100;
        if (p < 5) return 0;
        if (p < 15) return 1 + (rand() % 1000);
        if (p < 75) return (1 + (rand() % 1_000_000)) * 1 ether;
        if (p < 95) return 1 + (rand() % 1e27);
        if (p < 98) return (1 << 127) - 1 - (rand() % 1e6);
        return type(uint128).max - (rand() % 1e6);
    }

    /// Reward amounts: zero sometimes, otherwise up to about a thousand WIND.
    function randRewardAmount() internal returns (uint256) {
        return 1 + (rand() % (1000 ether));
    }

    function randDuration() internal returns (uint256) {
        uint256 p = rand() % 100;
        if (p < 8) return 0;
        if (p < 20) return rand() % (2 * WEEK);
        if (p < 50) return WEEK * (1 + (rand() % (MAXTIME / WEEK - 1)));
        if (p < 65) return MAXTIME - (rand() % (2 * WEEK));
        if (p < 80) return MAXTIME + (rand() % (3 * WEEK));
        if (p < 90) return WEEK * (1 + (rand() % (MAXTIME / WEEK)));
        return rand() % 1e12;
    }

    function randSplitAmount(uint256 f) internal returns (uint256) {
        IVotingEscrow.LockedBalance memory l = ve.locked(f);
        uint256 a = l.amount > 0 ? uint256(int256(l.amount)) : 1 ether;
        uint256 p = rand() % 100;
        if (p < 5) return 0;
        if (p < 15) return a + 1 + (rand() % 1 ether);
        if (p < 20) return type(uint128).max;
        return (a * (1 + (rand() % 99))) / 100;
    }

    function randPast() internal returns (uint256) {
        uint256 p = rand() % 100;
        if (p < 60) return T0 + (rand() % (clock - T0 + 1));
        if (p < 85) return ((T0 + (rand() % (clock - T0 + 1))) / WEEK) * WEEK;
        if (p < 95) return clock;
        return clock + (rand() % (2 * WEEK));
    }

    function ensureFunds(address who, uint256 amount) internal {
        if (wind.balanceOf(who) < amount) wind.testMint(who, amount);
    }

    function ensureTok(address token, address who, uint256 amount) internal {
        if (MockVeWind(token).balanceOf(who) < amount) MockVeWind(token).testMint(who, amount);
    }

    function rand() internal returns (uint256) {
        seed = uint256(keccak256(abi.encode(seed)));
        return seed;
    }

    /*//////////////////////////////////////////////////////////////
                                 SNAPSHOT
    //////////////////////////////////////////////////////////////*/

    /// Records the pre-call lock ends of the ids a call touches (`a`, `b`, `c`, and the next two ids a create
    /// or split mints), so `slopeChangePre` can be read for each.
    function snap3(uint256 a, uint256 b, uint256 c) internal {
        uint256 n = ve.tokenId();
        snapIds = [a, b, c, n + 1, n + 2];
        for (uint256 i = 0; i < 5; i++) {
            snapEnds[i] = ve.locked(snapIds[i]).end;
        }
    }

    function preEnd(uint256 id) internal view returns (uint256) {
        for (uint256 i = 0; i < 5; i++) {
            if (snapIds[i] == id) return snapEnds[i];
        }
        revert("snap miss");
    }

    /*//////////////////////////////////////////////////////////////
                                 LOGGING
    //////////////////////////////////////////////////////////////*/

    function logLine(
        string memory op,
        string memory args,
        bool ok,
        string memory out,
        uint256 a,
        uint256 b,
        uint256 c
    ) internal {
        vm.writeLine(
            F,
            string.concat(
                op,
                "|",
                s(clock),
                "|",
                s(blk),
                "|",
                args,
                "|",
                okStr(ok),
                "|",
                out,
                "|",
                gstate(),
                "|",
                tokList(a, b, c),
                "|",
                mList(a, b, c)
            )
        );
    }

    function dupOf(uint256[3] memory ids, uint256 i) internal pure returns (bool) {
        for (uint256 j = 0; j < i; j++) {
            if (ids[j] == ids[i]) return true;
        }
        return false;
    }

    /// Token entries for the given ids (zero means none, duplicates are skipped).
    function tokList(uint256 a, uint256 b, uint256 c) internal view returns (string memory r) {
        uint256[3] memory ids = [a, b, c];
        bool first = true;
        for (uint256 i = 0; i < 3; i++) {
            if (ids[i] == 0 || dupOf(ids, i)) continue;
            r = first ? tok(ids[i]) : string.concat(r, ";", tok(ids[i]));
            first = false;
        }
    }

    /// Managed entries for the given ids that are managed veNFTs.
    function mList(uint256 a, uint256 b, uint256 c) internal view returns (string memory r) {
        uint256[3] memory ids = [a, b, c];
        bool first = true;
        for (uint256 i = 0; i < 3; i++) {
            if (ids[i] == 0 || dupOf(ids, i)) continue;
            if (uint256(ve.escrowType(ids[i])) != 2) continue;
            r = first ? mEntry(ids[i]) : string.concat(r, ";", mEntry(ids[i]));
            first = false;
        }
    }

    function mEntry(uint256 m) internal view returns (string memory) {
        IReward l = IReward(ve.managedToLocked(m));
        IReward f = IReward(ve.managedToFree(m));
        return string.concat(
            s(m),
            ":",
            ve.deactivated(m) ? "1" : "0",
            ",",
            s(l.totalSupply()),
            ",",
            s(l.supplyNumCheckpoints()),
            ",",
            s(f.totalSupply()),
            ",",
            s(f.supplyNumCheckpoints())
        );
    }

    function tok(uint256 id) internal view returns (string memory) {
        return string.concat(tokCore(id), ",", tokManaged(id), ",", tokRewards(id));
    }

    function tokCore(uint256 id) internal view returns (string memory) {
        IVotingEscrow.LockedBalance memory l = ve.locked(id);
        uint256 ue = ve.userPointEpoch(id);
        IVotingEscrow.UserPoint memory up = ve.userPointHistory(id, ue);
        string memory head = string.concat(
            s(id), ":", a2s(ve.ownerOf(id)), ",", si(l.amount), ",", s(l.end), ",", b01(l.isPermanent), ",", s(ue)
        );
        string memory mid = string.concat(
            ",", si(up.bias), ",", si(up.slope), ",", s(up.ts), ",", s(up.blk), ",", s(up.permanent)
        );
        string memory slopes = string.concat(
            ",", si(ve.slopeChanges(preEnd(id))), ",", si(ve.slopeChanges(l.end))
        );
        return string.concat(head, mid, slopes);
    }

    function tokManaged(uint256 id) internal view returns (string memory) {
        uint256 m = ve.idToManaged(id);
        return string.concat(
            s(uint256(ve.escrowType(id))),
            ",",
            s(m),
            ",",
            s(ve.weights(id, m)),
            ",",
            s(ve.delegates(id)),
            ",",
            s(ve.numCheckpoints(id)),
            ",",
            cpsOf(id)
        );
    }

    function cpsOf(uint256 id) internal view returns (string memory out) {
        uint256 n = ve.numCheckpoints(id);
        if (n == 0) return "-";
        for (uint256 i = 0; i < n; i++) {
            IVotingEscrow.Checkpoint memory c = ve.checkpoints(id, uint48(i));
            string memory piece = string.concat(
                s(c.fromTimestamp), ".", a2s(c.owner), ".", s(c.delegatedBalance), ".", s(c.delegatee)
            );
            out = i == 0 ? piece : string.concat(out, "~", piece);
        }
    }

    function tokRewards(uint256 id) internal view returns (string memory) {
        uint256 dm = depositedM[id];
        if (dm == 0) return "0,0,0,0,0,0,0";
        IReward l = IReward(ve.managedToLocked(dm));
        IReward f = IReward(ve.managedToFree(dm));
        return string.concat(
            s(dm),
            ",",
            s(l.balanceOf(id)),
            ",",
            s(l.numCheckpoints(id)),
            ",",
            s(l.earned(address(wind), id)),
            ",",
            s(f.balanceOf(id)),
            ",",
            s(f.numCheckpoints(id)),
            ",",
            s(f.earned(address(wind), id))
        );
    }

    /// Global state: `epoch,supply,permanentLockBalance` then `pointHistory(epoch)`.
    function gstate() internal view returns (string memory) {
        uint256 e = ve.epoch();
        IVotingEscrow.GlobalPoint memory gp = ve.pointHistory(e);
        string memory head = string.concat(s(e), ",", s(ve.supply()), ",", s(ve.permanentLockBalance()), ",");
        return string.concat(
            head,
            si(gp.bias),
            ",",
            si(gp.slope),
            ",",
            s(gp.ts),
            ",",
            s(gp.blk),
            ",",
            s(gp.permanentLockBalance)
        );
    }

    /// Maps the revert data to a stable reason name.
    function reasonOf(bytes memory r) internal pure returns (string memory) {
        if (r.length < 4) return "empty";
        bytes32 w;
        assembly {
            w := mload(add(r, 32))
        }
        bytes4 sel = bytes4(w);
        if (sel == bytes4(0x4e487b71)) {
            uint256 code;
            assembly {
                code := mload(add(r, 36))
            }
            if (code == 0x11) return "Panic17";
            if (code == 0x12) return "Panic18";
            return "PanicOther";
        }
        string[45] memory names = [
            "AlreadyVoted",
            "AmountTooBig",
            "LockDurationNotInFuture",
            "LockDurationTooLong",
            "LockExpired",
            "LockNotExpired",
            "NoLockFound",
            "NotApprovedOrOwner",
            "NotNormalNFT",
            "NotOwner",
            "NotPermanentLock",
            "NotTeam",
            "NotVoter",
            "OwnershipChange",
            "PermanentLock",
            "SameNFT",
            "SplitNoOwner",
            "SplitNotAllowed",
            "ZeroAmount",
            "SafeCastOverflow",
            "SafeCastUnderflow",
            "NonExistentToken",
            "InvalidSignature",
            "InvalidSignatureS",
            "InvalidNonce",
            "SignatureExpired",
            "SameAddress",
            "ZeroAddress",
            "NotGovernor",
            "NotGovernorOrManager",
            "NotEmergencyCouncilOrGovernor",
            "NotManagedNFT",
            "NotLockedNFT",
            "NotManagedOrNormalNFT",
            "InvalidManagedNFTId",
            "SameState",
            "ZeroBalance",
            "NotDistributor",
            "NotAuthorized",
            "NotWhitelisted",
            "NotVotingEscrow",
            "NotSingleToken",
            "NotEscrowToken",
            "InvalidReward",
            "NotGauge"
        ];
        for (uint256 i = 0; i < names.length; i++) {
            if (sel == bytes4(keccak256(bytes(string.concat(names[i], "()"))))) return names[i];
        }
        return "unknown";
    }

    /*//////////////////////////////////////////////////////////////
                              FORMATTING
    //////////////////////////////////////////////////////////////*/

    function s(uint256 v) internal pure returns (string memory) {
        return vm.toString(v);
    }

    function si(int256 v) internal pure returns (string memory) {
        return vm.toString(v);
    }

    function a2s(address a) internal pure returns (string memory) {
        return vm.toString(uint256(uint160(a)));
    }

    function b01(bool v) internal pure returns (string memory) {
        return v ? "1" : "0";
    }

    function okStr(bool v) internal pure returns (string memory) {
        return v ? "ok" : "err";
    }
}
