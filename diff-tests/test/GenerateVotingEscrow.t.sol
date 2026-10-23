// SPDX-License-Identifier: GPL-3.0-or-later
pragma solidity ^0.8.19;

import {VotingEscrow} from "../windv2/contracts/VotingEscrow.sol";
import {IVotingEscrow} from "../windv2/contracts/interfaces/IVotingEscrow.sol";
import {MockVeWind} from "../src/ve/MockVeWind.sol";

interface Vm {
    function warp(uint256 newTimestamp) external;
    function roll(uint256 newHeight) external;
    function prank(address msgSender) external;
    function writeFile(string calldata path, string calldata data) external;
    function writeLine(string calldata path, string calldata data) external;
    function toString(uint256 value) external pure returns (string memory);
    function toString(int256 value) external pure returns (string memory);
}

/// @notice Generates differential test vectors from the real windv2 `VotingEscrow` (core lock logic).
/// Output: `vectors/voting_escrow.txt`, one line per call:
///
///   op|now|block|args|ok|out|G|T1;T2;...
///
/// - `now` / `block` are `block.timestamp` / `block.number` of the call.
/// - `args` is a comma-separated list. Addresses are their decimal `uint160` value. Layouts:
///   `init`: none. `createLock`: caller,value,duration. `createLockFor`: caller,to,value,duration.
///   `depositFor` / `increaseAmount`: caller,token,value. `increaseUnlockTime`: caller,token,duration.
///   `withdraw` / `lockPermanent` / `unlockPermanent`: caller,token. `merge`: caller,from,to.
///   `split`: caller,from,amount. `toggleSplit`: caller,account,flag. `transferFrom`: caller,from,to,token.
///   `voting`: caller,token,flag. `checkpoint`: none. `balanceOfNFT`: token.
///   `balanceOfNFTAt`: token,t. `totalSupply`: none. `totalSupplyAt`: t.
/// - `ok` is `ok` or `err`. `out` is the return value on success, or the revert reason on `err`.
///   `createLock(For)` returns `token:pulled` (WIND pulled into the escrow). `withdraw` returns the WIND
///   paid out to the owner. `split` returns `token1:token2`. Other successful writes return an empty string.
/// - `G` is the global state: `epoch,supply,permanentLockBalance,bias,slope,ts,blk,permanentLockBalance`
///   where the last five fields are `pointHistory(epoch)`.
/// - `T` is one entry per token touched by the call (the arg tokens, plus the new tokens on success), in
///   order: `id:owner,amount,end,isPermanent,userEpoch,bias,slope,ts,blk,permanent,slopeChangePre,slopeChangePost`.
///   `userEpoch`, `bias`, `slope`, `ts`, `blk`, `permanent` describe `userPointHistory(id, userPointEpoch(id))`.
///   `slopeChangePre` is `slopeChanges[end before the call]` and `slopeChangePost` is `slopeChanges[end after]`,
///   both read after the call.
/// - Revert reasons are the custom error names (`AlreadyVoted`, `LockExpired`, ...), `Panic17` for checked
///   arithmetic overflow or underflow, `SafeCastOverflow` / `SafeCastUnderflow` for the SafeCast library, or
///   `unknown` (the test then fails).
///
/// Setup: the constructor is `VotingEscrow(forwarder = 0, token = WIND, factoryRegistry = 0, team = TEAM)`.
/// `voter` is `team` in the constructor, so `voting` is called by pranking `TEAM`. No mocks are needed for
/// the voter. Users approve the escrow for an unlimited amount. Their WIND balances are topped up by the
/// generator when a call needs them. Those top-ups do not touch escrow state.
///
/// Run: `forge test --match-test testGenerateVotingEscrow`
contract GenerateVotingEscrowVectors {
    Vm constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

    uint256 constant STEPS = 3000;
    uint256 constant T0 = 1_780_000_000;
    uint256 constant B0 = 1_000;
    uint256 constant WEEK = 1 weeks;
    uint256 constant YEAR = 365 days;
    uint256 constant MAXTIME = 4 * 365 * 86400;
    uint256 constant NUSERS = 5;
    address constant TEAM = address(uint160(777));
    address constant OUTSIDER = address(uint160(5000));
    string constant F = "vectors/voting_escrow.txt";

    uint256 seed = 0x564f54; // "VOT"
    uint256 clock;
    uint256 blk;
    MockVeWind wind;
    VotingEscrow ve;
    address[5] users;
    // Token ids whose end is read before a call, and the end read at that time.
    uint256[4] snapIds;
    uint256[4] snapEnds;
    // Last token transferred successfully, so that same-block unlocks and reads hit `OwnershipChange`.
    uint256 lastMoved;

    function testGenerateVotingEscrow() public {
        vm.writeFile(F, "");
        deploy();
        logInit();
        toggle(TEAM, users[0], true);
        toggle(TEAM, users[1], true);
        toggle(TEAM, users[2], true);
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
        ve = new VotingEscrow(address(0), address(wind), address(0), TEAM);
        for (uint256 i = 0; i < NUSERS; i++) {
            users[i] = address(uint160(1000 + i));
            wind.testMint(users[i], 1 << 127);
            vm.prank(users[i]);
            wind.approve(address(ve), type(uint256).max);
        }
        wind.testMint(OUTSIDER, 1 << 127);
        vm.prank(OUTSIDER);
        wind.approve(address(ve), type(uint256).max);
    }

    function logInit() internal {
        vm.writeLine(F, string.concat("init|", s(clock), "|", s(blk), "||ok||", gstate(), "|"));
    }

    /*//////////////////////////////////////////////////////////////
                                 STEPS
    //////////////////////////////////////////////////////////////*/

    function step() internal {
        advance();
        uint256 p = rand() % 100;
        if (p < 10) doCreate(false);
        else if (p < 15) doCreate(true);
        else if (p < 23) doDeposit(false);
        else if (p < 31) doDeposit(true);
        else if (p < 36) doIncreaseUnlock();
        else if (p < 43) doWithdraw();
        else if (p < 50) doMerge();
        else if (p < 57) doSplit();
        else if (p < 60) doToggleRandom();
        else if (p < 65) doPermanent(true);
        else if (p < 70) doPermanent(false);
        else if (p < 76) doTransfer();
        else if (p < 79) doVoting();
        else if (p < 83) doCheckpoint();
        else if (p < 87) doReadBalance();
        else if (p < 91) doReadBalanceAt();
        else if (p < 95) doReadSupply();
        else doReadSupplyAt();
        require(wind.balanceOf(address(ve)) == ve.supply(), "conservation");
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

    /// Token for unlock and balance reads: the last transferred token 30% of the time (same-block
    /// transfer then unlock or read hits `OwnershipChange` / the zero-balance branch).
    function pickMoved() internal returns (uint256) {
        if (lastMoved != 0 && rand() % 100 < 30) return lastMoved;
        return pickToken();
    }

    /*//////////////////////////////////////////////////////////////
                                  OPS
    //////////////////////////////////////////////////////////////*/

    function doCreate(bool isFor) internal {
        address caller = pickActor(0);
        address to = isFor ? users[rand() % NUSERS] : caller;
        uint256 value = randValue();
        uint256 dur = randDuration();
        ensureFunds(caller, value);
        snap(0, 0);
        uint256 before = wind.balanceOf(address(ve));
        if (isFor) {
            string memory args = string.concat(a2s(caller), ",", a2s(to), ",", s(value), ",", s(dur));
            vm.prank(caller);
            try ve.createLockFor(value, dur, to) returns (uint256 id) {
                uint256 pulled = wind.balanceOf(address(ve)) - before;
                logLine("createLockFor", args, true, string.concat(s(id), ":", s(pulled)), id, 0, 0);
            } catch (bytes memory r) {
                logLine("createLockFor", args, false, reasonOf(r), 0, 0, 0);
            }
        } else {
            string memory args2 = string.concat(a2s(caller), ",", s(value), ",", s(dur));
            vm.prank(caller);
            try ve.createLock(value, dur) returns (uint256 id) {
                uint256 pulled2 = wind.balanceOf(address(ve)) - before;
                logLine("createLock", args2, true, string.concat(s(id), ":", s(pulled2)), id, 0, 0);
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
        snap(id, 0);
        uint256 before = wind.balanceOf(address(ve));
        string memory args = string.concat(a2s(caller), ",", s(id), ",", s(value));
        vm.prank(caller);
        if (increase) {
            try ve.increaseAmount(id, value) {
                logLine("increaseAmount", args, true, s(wind.balanceOf(address(ve)) - before), id, 0, 0);
            } catch (bytes memory r) {
                logLine("increaseAmount", args, false, reasonOf(r), id, 0, 0);
            }
        } else {
            try ve.depositFor(id, value) {
                logLine("depositFor", args, true, s(wind.balanceOf(address(ve)) - before), id, 0, 0);
            } catch (bytes memory r) {
                logLine("depositFor", args, false, reasonOf(r), id, 0, 0);
            }
        }
    }

    function doIncreaseUnlock() internal {
        uint256 id = pickToken();
        address caller = pickActor(id);
        uint256 dur = randDuration();
        snap(id, 0);
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
        snap(id, 0);
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
        if (r == 0) t = findOwnedPermanent(caller, f); // merge into a permanent lock (only `to` may be permanent)
        else if (r == 1) t = findOwned(caller, f);
        else t = pickToken();
        if (t == 0) t = pickToken();
        if (rand() % 25 == 0) t = f;
        snap(f, t);
        string memory args = string.concat(a2s(caller), ",", s(f), ",", s(t));
        vm.prank(caller);
        try ve.merge(f, t) {
            logLine("merge", args, true, "", f, t, 0);
        } catch (bytes memory r) {
            logLine("merge", args, false, reasonOf(r), f, t, 0);
        }
    }

    function doSplit() internal {
        uint256 f = rand() % 4 == 0 ? findPermanent() : pickToken();
        if (f == 0) f = pickToken();
        address caller = pickActor(f);
        uint256 amt = randSplitAmount(f);
        snap(f, 0);
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
        toggle(caller, account, rand() % 4 != 0); // mostly allow, so splits of permanent locks succeed
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
            snap(id, 0);
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
                snap(pid, 0);
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
        snap(id, 0);
        string memory args = string.concat(a2s(caller), ",", s(id));
        vm.prank(caller);
        try ve.unlockPermanent(id) {
            logLine("unlockPermanent", args, true, "", id, 0, 0);
        } catch (bytes memory r) {
            logLine("unlockPermanent", args, false, reasonOf(r), id, 0, 0);
        }
    }

    /// Some permanent token with an owner, or 0 when none.
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

    function doTransfer() internal {
        uint256 id = pickToken();
        address owner = id == 0 ? address(0) : ve.ownerOf(id);
        address from = rand() % 5 == 0 ? users[rand() % NUSERS] : owner;
        address caller = rand() % 10 == 0 ? users[rand() % NUSERS] : owner;
        if (caller == address(0)) caller = OUTSIDER;
        address to = users[rand() % NUSERS];
        snap(id, 0);
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
        address caller = rand() % 10 == 0 ? users[rand() % NUSERS] : TEAM;
        bool flag = rand() % 2 == 0;
        snap(id, 0);
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
                               PICKERS
    //////////////////////////////////////////////////////////////*/

    /// The owner of `id` 70% of the time when it exists, otherwise a user (or the outsider, 3%).
    function pickActor(uint256 id) internal returns (address) {
        uint256 r = rand() % 100;
        if (r < 70 && id != 0) {
            address o = ve.ownerOf(id);
            if (o != address(0)) return o;
        }
        if (r < 97) return users[rand() % NUSERS];
        return OUTSIDER;
    }

    /// Mostly an existing token. Sometimes not yet minted, sometimes id 0.
    function pickToken() internal returns (uint256) {
        uint256 n = ve.tokenId();
        uint256 r = rand() % 100;
        if (r == 3) return 0;
        if (n == 0 || r < 3) return n + 1 + (rand() % 2);
        return 1 + (rand() % n);
    }

    /// Some token owned by `owner` other than `avoid`, or 0 when none.
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

    /// Some permanent token owned by `owner` other than `avoid`, or 0 when none.
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

    function randValue() internal returns (uint256) {
        uint256 p = rand() % 100;
        if (p < 5) return 0;
        if (p < 15) return 1 + (rand() % 1000);
        if (p < 75) return (1 + (rand() % 1_000_000)) * 1 ether;
        if (p < 95) return 1 + (rand() % 1e27);
        if (p < 98) return (1 << 127) - 1 - (rand() % 1e6);
        return type(uint128).max - (rand() % 1e6);
    }

    /// Lock durations: zero, short, up to MAXTIME, around MAXTIME, and beyond it (reverts).
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

    /// Split amounts: zero, above the lock amount, above int128, or a fraction of the lock amount.
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

    function rand() internal returns (uint256) {
        seed = uint256(keccak256(abi.encode(seed)));
        return seed;
    }

    /*//////////////////////////////////////////////////////////////
                                 LOGGING
    //////////////////////////////////////////////////////////////*/

    /// Records the touched tokens' pre-call ends so `tok` can report slope changes at the old end.
    /// Candidates are `a`, `b`, and the next two token ids (the ones a create or split mints).
    function snap(uint256 a, uint256 b) internal {
        uint256 n = ve.tokenId();
        snapIds = [a, b, n + 1, n + 2];
        for (uint256 i = 0; i < 4; i++) {
            snapEnds[i] = ve.locked(snapIds[i]).end;
        }
    }

    function preEnd(uint256 id) internal view returns (uint256) {
        for (uint256 i = 0; i < 4; i++) {
            if (snapIds[i] == id) return snapEnds[i];
        }
        revert("snap miss");
    }

    function logLine(
        string memory op,
        string memory args,
        bool ok,
        string memory out,
        uint256 a,
        uint256 b,
        uint256 c
    ) internal {
        string memory toks = tokList(a, b, c);
        vm.writeLine(
            F,
            string.concat(op, "|", s(clock), "|", s(blk), "|", args, "|", okStr(ok), "|", out, "|", gstate(), "|", toks)
        );
    }

    /// Token entries for the given ids (zero means none, duplicates are skipped).
    function tokList(uint256 a, uint256 b, uint256 c) internal view returns (string memory r) {
        uint256[3] memory ids = [a, b, c];
        bool first = true;
        for (uint256 i = 0; i < 3; i++) {
            uint256 id = ids[i];
            if (id == 0) continue;
            bool dup = false;
            for (uint256 j = 0; j < i; j++) {
                if (ids[j] == id) dup = true;
            }
            if (dup) continue;
            r = first ? tok(id) : string.concat(r, ";", tok(id));
            first = false;
        }
    }

    function tok(uint256 id) internal view returns (string memory) {
        IVotingEscrow.LockedBalance memory l = ve.locked(id);
        uint256 ue = ve.userPointEpoch(id);
        IVotingEscrow.UserPoint memory up = ve.userPointHistory(id, ue);
        string memory head = string.concat(
            s(id), ":", a2s(ve.ownerOf(id)), ",", si(l.amount), ",", s(l.end), ",", b01(l.isPermanent), ","
        );
        string memory mid = string.concat(
            s(ue), ",", si(up.bias), ",", si(up.slope), ",", s(up.ts), ",", s(up.blk), ",", s(up.permanent), ","
        );
        return string.concat(head, mid, si(ve.slopeChanges(preEnd(id))), ",", si(ve.slopeChanges(l.end)));
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
        string[22] memory names = [
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
            "NonExistentToken"
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
