// SPDX-License-Identifier: GPL-3.0-or-later
pragma solidity ^0.8.19;

import {VotingEscrow} from "../windv2/contracts/VotingEscrow.sol";
import {IVotingEscrow} from "../windv2/contracts/interfaces/IVotingEscrow.sol";
import {RewardsDistributor} from "../windv2/contracts/RewardsDistributor.sol";
import {MockVeWind} from "../src/ve/MockVeWind.sol";
import {MockRebaseMinter} from "../src/rebase/MockRebaseMinter.sol";

interface Vm {
    function warp(uint256 newTimestamp) external;
    function roll(uint256 newHeight) external;
    function prank(address msgSender) external;
    function writeFile(string calldata path, string calldata data) external;
    function writeLine(string calldata path, string calldata data) external;
    function toString(uint256 value) external pure returns (string memory);
    function toString(int256 value) external pure returns (string memory);
    function toString(bytes32 value) external pure returns (string memory);
}

/// @notice Generates differential test vectors from the real windv2 `VotingEscrow` and `RewardsDistributor`,
/// with a mock WIND token and two mock minters (`MockRebaseMinter`) that forward rebases and `setMinter`.
/// Output: `vectors/rebase.txt`, one line per call:
///
///   op|now|block|args|ok|out|G|T
///
/// - `now` / `block` are `block.timestamp` / `block.number` of the call (`block` is also what the escrow reads).
/// - `args`: comma-separated decimal values. Addresses are their `uint160` value. Minters are 0 / 1 (indexes
///   into the two mocks). Layouts:
///   `createLock`: caller,value,duration. `createLockFor`: caller,to,value,duration.
///   `depositFor` / `increaseAmount`: caller,token,value. `increaseUnlockTime`: caller,token,duration.
///   `withdraw` / `lockPermanent` / `unlockPermanent`: caller,token. `transferFrom`: caller,from,to,token.
///   `rebase`: minter,amount. `setMinter`: callerMinter,newMinter. `updatePeriod`: period.
///   `claim`: token. `claimMany`: token,token,...
/// - `ok` is `ok` or `err`. `out` on success: `createLock(For)` returns `token:pulled`, `depositFor` /
///   `increaseAmount` return the amount pulled, `withdraw` and `claim` return the WIND paid, `claimMany`
///   returns `true`, others are empty. On `err`, `out` is the revert reason.
/// - `G` is `lastTokenTime,tokenLastBalance,minter,activePeriod,digest,supply,epoch,dist,ve,u0..u4,outsider`
///   where `minter` is 0 or 1, `digest` is `keccak256` of `tokensPerWeek[w]` (32-byte big-endian) for every week
///   `w` from `startTime` to the week of `now`, and the balances are WIND balances.
/// - `T` is one entry per token `1..tokenId`, in order:
///   `id:owner,amount,end,isPermanent,userEpoch,bias,slope,ts,blk,permanent,timeCursorOf,claimable`,
///   separated by `;`. `owner` is 0 once burnt.
///
/// Time: each step advances the clock (mostly short steps, some week-long ones). Step 300 jumps 21 weeks and
/// rebases (the checkpoint 20-week cap). Step 1000 jumps 52 weeks and rebases, step 1001 forces `updatePeriod`,
/// and step 1002 claims token 1. Steps 1100..1105 are a scripted burn-with-rewards sequence (see below). Token 1 is held back from claims and withdrawals in steps 100..1001, so its
/// claim cursor is more than 50 weeks behind `lastTokenTime` (the 50-week claim cap).
///
/// Run: `forge test --match-test testGenerateRebase`
contract GenerateRebaseVectors {
    Vm constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

    uint256 constant STEPS = 1500;
    uint256 constant T0 = 1_780_000_000;
    uint256 constant B0 = 1_000;
    uint256 constant WEEK = 1 weeks;
    uint256 constant MAXTIME = 4 * 365 * 86400;
    uint256 constant NUSERS = 5;
    uint256 constant MAXTOKENS = 6; // live (not burnt) veNFTs
    uint256 constant HOLD_FROM = 100; // token 1 is not claimed or withdrawn in [HOLD_FROM, HOLD_TO)
    uint256 constant HOLD_TO = 1002;
    address constant TEAM = address(uint160(777));
    address constant OUTSIDER = address(uint160(5000));
    string constant F = "vectors/rebase.txt";

    uint256 seed = 0x5245424153; // "REBAS"
    uint256 clock;
    uint256 blk;
    uint256 step;
    MockVeWind wind;
    VotingEscrow ve;
    RewardsDistributor dist;
    MockRebaseMinter[2] minters;
    address[5] users;
    uint256 lastCreated; // the token opened by the scripted burn-with-rewards sequence

    function testGenerateRebase() public {
        vm.writeFile(F, "");
        deploy();
        logInit();
        for (step = 0; step < STEPS; step++) {
            doStep();
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
        minters[0] = new MockRebaseMinter(wind);
        minters[1] = new MockRebaseMinter(wind);
        dist = new RewardsDistributor(address(ve), address(minters[0]));
        for (uint256 i = 0; i < 2; i++) {
            minters[i].setActivePeriod((clock / WEEK) * WEEK);
        }
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

    function doStep() internal {
        advance();
        if (step == 300 || step == 1000) return doRebase(true);
        if (step == 1001) return doUpdatePeriod(true);
        if (step == 1002) return doClaimOne(1);
        // Scripted sequence (steps 1100..1105): a 2-week lock earns rewards while live, then is withdrawn after
        // expiry without a claim. Claiming the burnt token must revert (zero owner).
        if (step == 1100) return doScriptedCreate();
        if (step == 1101) return doRebaseAmount(1e21);
        if (step == 1102) return doRebaseAmount(0);
        if (step == 1103) return doWithdrawId(lastCreated, users[0]);
        if (step == 1104) return doUpdatePeriod(true);
        if (step == 1105) return doClaimOne(lastCreated);

        uint256 p = rand() % 1000;
        if (p < 60) doCreate();
        else if (p < 80) doCreateFor();
        else if (p < 190) doDeposit();
        else if (p < 250) doIncreaseAmount();
        else if (p < 290) doIncreaseUnlock();
        else if (p < 330) doWithdraw();
        else if (p < 350) doPermanent(true);
        else if (p < 370) doPermanent(false);
        else if (p < 420) doTransfer();
        else if (p < 490) doRebase(false);
        else if (p < 520) doSetMinter();
        else if (p < 620) doUpdatePeriod(false);
        else if (p < 760) doClaimOne(claimPick());
        else doClaimMany();
        require(wind.balanceOf(address(ve)) == ve.supply(), "conservation");
    }

    /// Advances the clock. Mostly short steps, some week-long ones. Steps 300 and 1000 are forced long gaps.
    function advance() internal {
        uint256 d;
        if (step == 300) {
            d = 21 * WEEK + (rand() % WEEK);
        } else if (step == 1000) {
            d = 52 * WEEK + (rand() % WEEK);
        } else if (step >= 1100 && step <= 1105) {
            d = (step == 1101 || step == 1102 || step == 1103) ? WEEK : 1 hours;
        } else {
            uint256 p = rand() % 100;
            if (p < 80) d = 1 + (rand() % (WEEK / 50));
            else if (p < 92) d = 1 + (rand() % (WEEK / 5));
            else d = 1 + (rand() % (WEEK / 2));
        }
        clock += d;
        vm.warp(clock);
        blk += rand() % 3;
        vm.roll(blk);
    }

    function doCreate() internal {
        if (liveTokens() >= MAXTOKENS) return doDeposit();
        address caller = pickUser();
        uint256 v = randValue();
        uint256 d = randDuration();
        string memory args = string.concat(a2s(caller), ",", s(v), ",", s(d));
        vm.prank(caller);
        try ve.createLock(v, d) returns (uint256 id) {
            logLine("createLock", args, true, string.concat(s(id), ":", s(v)));
        } catch (bytes memory r) {
            logLine("createLock", args, false, reasonOf(r));
        }
    }

    function doCreateFor() internal {
        if (liveTokens() >= MAXTOKENS) return doDeposit();
        address caller = pickUser();
        address to = users[rand() % NUSERS];
        uint256 v = randValue();
        uint256 d = randDuration();
        string memory args = string.concat(a2s(caller), ",", a2s(to), ",", s(v), ",", s(d));
        vm.prank(caller);
        try ve.createLockFor(v, d, to) returns (uint256 id) {
            logLine("createLockFor", args, true, string.concat(s(id), ":", s(v)));
        } catch (bytes memory r) {
            logLine("createLockFor", args, false, reasonOf(r));
        }
    }

    function doDeposit() internal {
        uint256 id = pickToken();
        address caller = pickUser();
        uint256 v = randValue();
        string memory args = string.concat(a2s(caller), ",", s(id), ",", s(v));
        vm.prank(caller);
        try ve.depositFor(id, v) {
            logLine("depositFor", args, true, s(v));
        } catch (bytes memory r) {
            logLine("depositFor", args, false, reasonOf(r));
        }
    }

    function doIncreaseAmount() internal {
        uint256 id = pickToken();
        address caller = pickOwnerOr(id);
        uint256 v = randValue();
        string memory args = string.concat(a2s(caller), ",", s(id), ",", s(v));
        vm.prank(caller);
        try ve.increaseAmount(id, v) {
            logLine("increaseAmount", args, true, s(v));
        } catch (bytes memory r) {
            logLine("increaseAmount", args, false, reasonOf(r));
        }
    }

    function doIncreaseUnlock() internal {
        uint256 id = pickToken();
        address caller = pickOwnerOr(id);
        uint256 d = randDuration();
        string memory args = string.concat(a2s(caller), ",", s(id), ",", s(d));
        vm.prank(caller);
        try ve.increaseUnlockTime(id, d) {
            logLine("increaseUnlockTime", args, true, "");
        } catch (bytes memory r) {
            logLine("increaseUnlockTime", args, false, reasonOf(r));
        }
    }

    function doWithdraw() internal {
        uint256 id = rand() % 100 < 60 ? pickMatching(2) : 0;
        if (id == 0) id = pickToken();
        doWithdrawId(id, pickOwnerOr(id));
    }

    function doPermanent(bool lock_) internal {
        uint256 id = pickToken();
        address caller = pickOwnerOr(id);
        string memory args = string.concat(a2s(caller), ",", s(id));
        vm.prank(caller);
        if (lock_) {
            try ve.lockPermanent(id) {
                logLine("lockPermanent", args, true, "");
            } catch (bytes memory r) {
                logLine("lockPermanent", args, false, reasonOf(r));
            }
        } else {
            try ve.unlockPermanent(id) {
                logLine("unlockPermanent", args, true, "");
            } catch (bytes memory r) {
                logLine("unlockPermanent", args, false, reasonOf(r));
            }
        }
    }

    function doTransfer() internal {
        uint256 id = pickToken();
        address from = ownerOrUser(id);
        address to = users[rand() % NUSERS];
        string memory args = string.concat(a2s(from), ",", a2s(from), ",", a2s(to), ",", s(id));
        vm.prank(from);
        try ve.transferFrom(from, to, id) {
            logLine("transferFrom", args, true, "");
        } catch (bytes memory r) {
            logLine("transferFrom", args, false, reasonOf(r));
        }
    }

    function doScriptedCreate() internal {
        string memory args = string.concat(a2s(users[0]), ",", s(1e21), ",", s(2 * WEEK));
        vm.prank(users[0]);
        uint256 id = ve.createLock(1e21, 2 * WEEK);
        lastCreated = id;
        logLine("createLock", args, true, string.concat(s(id), ":", s(1e21)));
    }

    function doWithdrawId(uint256 id, address caller) internal {
        string memory args = string.concat(a2s(caller), ",", s(id));
        uint256 before = wind.balanceOf(caller);
        vm.prank(caller);
        try ve.withdraw(id) {
            logLine("withdraw", args, true, s(wind.balanceOf(caller) - before));
        } catch (bytes memory r) {
            logLine("withdraw", args, false, reasonOf(r));
        }
    }

    /// Rebase: the current minter 85% of the time (the other one otherwise, which must revert `NotMinter`).
    function doRebase(bool forced) internal {
        uint256 cur = curMinter();
        uint256 idx = (forced || rand() % 100 < 85) ? cur : 1 - cur;
        doRebaseFrom(idx, forced ? 1e21 : randRebase());
    }

    function doRebaseAmount(uint256 amount) internal {
        doRebaseFrom(curMinter(), amount);
    }

    function doRebaseFrom(uint256 idx, uint256 amount) internal {
        wind.testMint(address(minters[idx]), amount);
        string memory args = string.concat(s(idx), ",", s(amount));
        try minters[idx].rebase(address(dist), amount) {
            logLine("rebase", args, true, "");
        } catch (bytes memory r) {
            logLine("rebase", args, false, reasonOf(r));
        }
    }

    function doSetMinter() internal {
        uint256 cur = curMinter();
        uint256 callerIdx = rand() % 100 < 90 ? cur : 1 - cur;
        uint256 newIdx = rand() % 2;
        string memory args = string.concat(s(callerIdx), ",", s(newIdx));
        try minters[callerIdx].handOver(address(dist), address(minters[newIdx])) {
            logLine("setMinter", args, true, "");
        } catch (bytes memory r) {
            logLine("setMinter", args, false, reasonOf(r));
        }
    }

    /// `Minter.updatePeriod` stand-in: the current week (80%) or the previous one (stale, so claims revert).
    function doUpdatePeriod(bool forced) internal {
        uint256 week = (clock / WEEK) * WEEK;
        uint256 period = (!forced && rand() % 100 < 10) ? week - WEEK : week;
        minters[0].setActivePeriod(period);
        minters[1].setActivePeriod(period);
        logLine("updatePeriod", s(period), true, "");
    }

    function doClaimOne(uint256 id) internal {
        string memory args = s(id);
        try dist.claim(id) returns (uint256 amount) {
            logLine("claim", args, true, s(amount));
        } catch (bytes memory r) {
            logLine("claim", args, false, reasonOf(r));
        }
    }

    function doClaimMany() internal {
        uint256 n = 1 + (rand() % 6);
        uint256[] memory ids = new uint256[](n);
        string memory args = "";
        for (uint256 i = 0; i < n; i++) {
            uint256 r = rand() % 100;
            if (r < 5) {
                ids[i] = 0; // stops the loop (`break`)
            } else if (r < 25 && pickMatching(1) != 0) {
                ids[i] = pickMatching(1); // a burnt token: its payout reverts
            } else {
                ids[i] = pickToken();
            }
            args = i == 0 ? s(ids[i]) : string.concat(args, ",", s(ids[i]));
        }
        try dist.claimMany(ids) returns (bool) {
            logLine("claimMany", args, true, "true");
        } catch (bytes memory r) {
            logLine("claimMany", args, false, reasonOf(r));
        }
    }

    /*//////////////////////////////////////////////////////////////
                                PICKERS
    //////////////////////////////////////////////////////////////*/

    /// Token id: zero or a missing id 5% and 5%, otherwise a random existing token. Token 1 is held back
    /// between steps HOLD_FROM and HOLD_TO so its claim cursor falls behind (the 50-week cap test).
    function pickToken() internal returns (uint256) {
        uint256 n = ve.tokenId();
        uint256 r = rand() % 100;
        if (n == 0 || r < 5) return 0;
        if (r < 10) return n + 1;
        uint256 id = 1 + (rand() % n);
        if (id == 1 && step >= HOLD_FROM && step < HOLD_TO) id = n >= 2 ? 2 : 1;
        return id;
    }

    /// Claim target: a burnt token 20% (the zero-owner revert), an expired live lock 60% (a transfer payout)
    /// of the rest, else any token.
    function claimPick() internal returns (uint256) {
        uint256 r = rand() % 100;
        if (r < 20) {
            uint256 b = pickMatching(1);
            if (b != 0) return b;
        } else if (r < 45) {
            uint256 e = pickMatching(2);
            if (e != 0) return e;
        }
        return pickToken();
    }

    /// A random token of the given kind: 1 burnt (zero owner), 2 expired, non-permanent and live. Zero if none.
    function pickMatching(uint256 kind) internal returns (uint256) {
        uint256 n = ve.tokenId();
        uint256 count;
        for (uint256 id = 1; id <= n; id++) {
            if (matches(id, kind)) count++;
        }
        if (count == 0) return 0;
        uint256 pick = rand() % count;
        for (uint256 id = 1; id <= n; id++) {
            if (matches(id, kind)) {
                if (pick == 0) return id;
                pick--;
            }
        }
        return 0;
    }

    function matches(uint256 id, uint256 kind) internal view returns (bool) {
        if (id == 1 && step >= HOLD_FROM && step < HOLD_TO) return false; // held back for the cap test
        IVotingEscrow.LockedBalance memory l = ve.locked(id);
        if (kind == 1) return ve.ownerOf(id) == address(0);
        return ve.ownerOf(id) != address(0) && !l.isPermanent && l.end <= clock && l.amount > 0;
    }

    /// Tokens that still have an owner (a withdrawn token is burnt and frees a slot).
    function liveTokens() internal view returns (uint256 live) {
        uint256 n = ve.tokenId();
        for (uint256 id = 1; id <= n; id++) {
            if (ve.ownerOf(id) != address(0)) live++;
        }
    }

    function pickUser() internal returns (address) {
        if (rand() % 100 < 5) return OUTSIDER;
        return users[rand() % NUSERS];
    }

    /// The owner of `id` 80% of the time, else a random user.
    function pickOwnerOr(uint256 id) internal returns (address) {
        if (rand() % 100 < 80 && id != 0) {
            address o = ve.ownerOf(id);
            if (o != address(0)) return o;
        }
        return pickUser();
    }

    function ownerOrUser(uint256 id) internal returns (address) {
        address o = id == 0 ? address(0) : ve.ownerOf(id);
        return o == address(0) ? users[rand() % NUSERS] : o;
    }

    function curMinter() internal view returns (uint256) {
        return dist.minter() == address(minters[1]) ? 1 : 0;
    }

    /// Mix of zero, small, large and overflowing values. Every user holds 2^127, so balances last.
    function randValue() internal returns (uint256) {
        uint256 p = rand() % 100;
        if (p < 5) return 0;
        if (p < 15) return 1 + (rand() % 1000);
        if (p < 75) return (1 + (rand() % 1_000_000)) * 1 ether;
        if (p < 92) return 1 + (rand() % 1e27);
        if (p < 96) return 1 << 127; // above int128 max: SafeCastOverflow
        return type(uint128).max - (rand() % 1e6);
    }

    /// Lock durations: zero, short (a few weeks, so locks expire and can be withdrawn), a random multi-week
    /// duration, around MAXTIME, and beyond MAXTIME (reverts).
    function randDuration() internal returns (uint256) {
        uint256 p = rand() % 100;
        if (p < 6) return 0;
        if (p < 46) return WEEK * (1 + (rand() % 4));
        if (p < 56) return WEEK * (1 + (rand() % (MAXTIME / WEEK - 1)));
        if (p < 66) return MAXTIME - (rand() % (2 * WEEK));
        if (p < 74) return MAXTIME + (rand() % (3 * WEEK));
        if (p < 90) return rand() % (2 * WEEK);
        return WEEK * (1 + (rand() % (MAXTIME / WEEK)));
    }

    function randRebase() internal returns (uint256) {
        uint256 p = rand() % 100;
        if (p < 5) return 0;
        if (p < 25) return 1 + (rand() % 1_000_000);
        if (p < 85) return (1 + (rand() % 1_000_000)) * 1 ether;
        return 1 + (rand() % 1e24);
    }

    function rand() internal returns (uint256) {
        seed = uint256(keccak256(abi.encode(seed)));
        return seed;
    }

    /*//////////////////////////////////////////////////////////////
                                 LOGGING
    //////////////////////////////////////////////////////////////*/

    function logLine(string memory op, string memory args, bool ok, string memory out) internal {
        vm.writeLine(
            F,
            string.concat(
                op, "|", s(clock), "|", s(blk), "|", args, "|", ok ? "ok" : "err", "|", out, "|", gstate(), "|", tokens()
            )
        );
    }

    function gstate() internal view returns (string memory) {
        string memory head = string.concat(
            s(dist.lastTokenTime()), ",", s(dist.tokenLastBalance()), ",", s(curMinter()), ",", s(minters[0].activePeriod()), ","
        );
        string memory mid = string.concat(digest(), ",", s(ve.supply()), ",", s(ve.epoch()), ",", s(wind.balanceOf(address(dist))));
        return string.concat(head, mid, balances());
    }

    /// `ve`, then the five users and the outsider.
    function balances() internal view returns (string memory) {
        string memory a = string.concat(s(wind.balanceOf(address(ve))), ",", s(wind.balanceOf(users[0])), ",", s(wind.balanceOf(users[1])));
        string memory b = string.concat(s(wind.balanceOf(users[2])), ",", s(wind.balanceOf(users[3])), ",", s(wind.balanceOf(users[4])));
        return string.concat(",", a, ",", b, ",", s(wind.balanceOf(OUTSIDER)));
    }

    /// `keccak256` of `tokensPerWeek[w]` (32-byte big-endian) for every week from `startTime` to the week of `clock`.
    function digest() internal view returns (string memory) {
        uint256 start = dist.startTime();
        uint256 end = (clock / WEEK) * WEEK;
        uint256[] memory vals = new uint256[]((end - start) / WEEK + 1);
        for (uint256 i = 0; i < vals.length; i++) {
            vals[i] = dist.tokensPerWeek(start + i * WEEK);
        }
        // `abi.encodePacked` of a uint256 array is the 32-byte big-endian words back to back.
        return vm.toString(keccak256(abi.encodePacked(vals)));
    }

    function tokens() internal view returns (string memory r) {
        uint256 n = ve.tokenId();
        for (uint256 id = 1; id <= n; id++) {
            string memory entry = tok(id);
            r = id == 1 ? entry : string.concat(r, ";", entry);
        }
    }

    function tok(uint256 id) internal view returns (string memory) {
        IVotingEscrow.LockedBalance memory l = ve.locked(id);
        uint256 ue = ve.userPointEpoch(id);
        IVotingEscrow.UserPoint memory up = ve.userPointHistory(id, ue);
        string memory a = string.concat(s(id), ":", a2s(ve.ownerOf(id)), ",", si(l.amount), ",", s(l.end), ",", l.isPermanent ? "1" : "0");
        string memory b = string.concat(s(ue), ",", si(up.bias), ",", si(up.slope), ",", s(up.ts), ",", s(up.blk), ",", s(up.permanent));
        string memory c = string.concat(s(dist.timeCursorOf(id)), ",", s(dist.claimable(id)));
        return string.concat(a, ",", b, ",", c);
    }

    /// Maps revert data to a stable reason name. Custom errors by name, `Panic17` / `Panic18` for checked
    /// arithmetic and division, and `Error(string)` for the OpenZeppelin zero-address transfer.
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
        if (sel == bytes4(keccak256("Error(string)"))) {
            bytes memory body = new bytes(r.length - 4);
            for (uint256 i = 0; i < body.length; i++) {
                body[i] = r[i + 4];
            }
            string memory m = abi.decode(body, (string));
            if (keccak256(bytes(m)) == keccak256("ERC20: transfer to the zero address")) {
                return "TransferToZeroAddress";
            }
            return "unknownString";
        }
        string[25] memory names = [
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
            "NotMinter",
            "UpdatePeriod",
            "NotManagedOrNormalNFT"
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

    /// Decimal `uint160` value of an address (the form the Rust side compares).
    function a2s(address a) internal pure returns (string memory) {
        return vm.toString(uint256(uint160(a)));
    }
}
