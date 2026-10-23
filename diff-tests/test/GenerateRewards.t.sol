// SPDX-License-Identifier: GPL-3.0-or-later
pragma solidity ^0.8.19;

import {FeesVotingReward} from "../windv2/contracts/rewards/FeesVotingReward.sol";
import {BribeVotingReward} from "../windv2/contracts/rewards/BribeVotingReward.sol";
import {IReward} from "../windv2/contracts/interfaces/IReward.sol";
import {RewardMockToken} from "../src/rewards/RewardMockToken.sol";
import {RewardMockVoter} from "../src/rewards/RewardMockVoter.sol";
import {RewardMockVotingEscrow} from "../src/rewards/RewardMockVotingEscrow.sol";

interface Vm {
    function warp(uint256 newTimestamp) external;
    function prank(address msgSender) external;
    function writeFile(string calldata path, string calldata data) external;
    function writeLine(string calldata path, string calldata data) external;
    function toString(uint256 value) external pure returns (string memory);
}

/// @notice Generates differential test vectors from the real windv2 `FeesVotingReward` and
/// `BribeVotingReward` (deployed with mocks for voter, voting escrow and reward tokens).
/// Output: `vectors/rewards.txt`, one line per call. Formats (fields separated by `|`):
///  - `init|C|now|rewards|state`                       C = F (fees) or B (bribe)
///  - `deposit|C|now|tokenId|amount|auth|ok|-|state`
///  - `withdraw|C|now|tokenId|amount|auth|ok|-|state`
///  - `notify|C|now|token|amount|flag|ok|-|state`      flag = isGauge (F) or whitelisted (B)
///  - `getreward|C|now|tokenId|tokens|caller|ap|ok|out|state`
///      caller = o (owner), a (approved), v (voter), s (stranger); ap = isApprovedOrOwner(caller, tokenId);
///      out = `t0=<delta>;...;t4=<delta>`, the payout received by the owner per token (balance delta)
///  - `earned|C|now|token|tokenId|ok|value`
/// Failing calls use `err|reason` in place of `ok|out` and keep the state field. `reason` is
/// `NotAuthorized`, `NotGauge`, `InvalidReward`, `NotWhitelisted`, `ZeroAmount`, `Panic17` (arithmetic
/// overflow or underflow), `Panic18` (division by zero) or `PanicOther`.
/// `state` is `totalSupply,supplyNumCheckpoints,rewardsListLength,isRewardBits(t0..t4),` then
/// `balanceOf(1..6)`, `numCheckpoints(1..6)`, `lastEarn(t0,1..6),...,lastEarn(t4,1..6)`,
/// `tokenRewardsPerEpoch(t0..t4, epochStart(now))`, all decimal and comma separated.
/// Run: `forge test --match-test testGenerateRewards`
contract GenerateRewardsVectors {
    Vm constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

    uint256 constant OPS = 3000;
    uint256 constant T0 = 1_780_000_000;
    uint256 constant WEEK = 1 weeks;
    uint256 constant NTOK = 5;
    uint256 constant NID = 6;
    address constant GAUGE = address(0x6A06E);
    address constant BRIBER = address(0xB21BE);
    address constant OTHER = address(0x0DD);
    address constant APPROVED = address(0xA99);
    address constant FORWARDER = address(0xF0F0);

    uint256 seed = 0x52455744; // "REWD"
    uint256 clock;

    RewardMockToken[5] tokens;
    RewardMockVoter voter;
    RewardMockVotingEscrow ve;
    FeesVotingReward fees;
    BribeVotingReward bribe;

    function rand() internal returns (uint256) {
        seed = uint256(keccak256(abi.encode(seed)));
        return seed;
    }

    function s(uint256 v) internal pure returns (string memory) {
        return vm.toString(v);
    }

    function b(bool v) internal pure returns (string memory) {
        return v ? "1" : "0";
    }

    function tag(bool isFees) internal pure returns (string memory) {
        return isFees ? "F" : "B";
    }

    function tokenName(uint256 j) internal pure returns (string memory) {
        return string.concat("t", s(j));
    }

    function owner(uint256 tid) internal pure returns (address) {
        return address(uint160(0x1000 + tid));
    }

    function rewardOf(bool isFees) internal view returns (IReward) {
        return isFees ? IReward(address(fees)) : IReward(address(bribe));
    }

    /// Full observable state of a reward contract at epoch `t` (see the header for the layout).
    function state(IReward r, uint256 t) internal view returns (string memory out) {
        out = string.concat(
            s(r.totalSupply()), ",", s(r.supplyNumCheckpoints()), ",", s(r.rewardsListLength()), ","
        );
        for (uint256 j = 0; j < NTOK; j++) {
            out = string.concat(out, b(r.isReward(address(tokens[j]))));
        }
        for (uint256 i = 1; i <= NID; i++) {
            out = string.concat(out, ",", s(r.balanceOf(i)));
        }
        for (uint256 i = 1; i <= NID; i++) {
            out = string.concat(out, ",", s(r.numCheckpoints(i)));
        }
        for (uint256 j = 0; j < NTOK; j++) {
            for (uint256 i = 1; i <= NID; i++) {
                out = string.concat(out, ",", s(r.lastEarn(address(tokens[j]), i)));
            }
        }
        uint256 ep = t - (t % WEEK);
        for (uint256 j = 0; j < NTOK; j++) {
            out = string.concat(out, ",", s(r.tokenRewardsPerEpoch(address(tokens[j]), ep)));
        }
    }

    /// Maps the revert data of a failed call to a stable reason name.
    function reasonOf(bytes memory r) internal pure returns (string memory) {
        if (r.length < 4) return "empty";
        bytes32 w;
        assembly {
            w := mload(add(r, 32))
        }
        bytes4 sel = bytes4(w);
        if (sel == IReward.NotAuthorized.selector) return "NotAuthorized";
        if (sel == IReward.NotGauge.selector) return "NotGauge";
        if (sel == IReward.InvalidReward.selector) return "InvalidReward";
        if (sel == IReward.NotWhitelisted.selector) return "NotWhitelisted";
        if (sel == IReward.ZeroAmount.selector) return "ZeroAmount";
        if (sel == bytes4(0x4e487b71)) {
            uint256 code;
            assembly {
                code := mload(add(r, 36))
            }
            if (code == 0x11) return "Panic17";
            if (code == 0x12) return "Panic18";
            return "PanicOther";
        }
        return "unknown";
    }

    function testGenerateRewards() public {
        string memory f = "vectors/rewards.txt";
        vm.writeFile(f, "");
        deployAll();
        vm.writeLine(f, string.concat("init|F|", s(clock), "|t0,t1,t2|", state(IReward(address(fees)), clock)));
        vm.writeLine(f, string.concat("init|B|", s(clock), "|t0|", state(IReward(address(bribe)), clock)));
        for (uint256 i = 0; i < OPS; i++) {
            step(f);
        }
    }

    function deployAll() internal {
        clock = T0;
        vm.warp(clock);
        for (uint256 j = 0; j < NTOK; j++) {
            tokens[j] = new RewardMockToken(tokenName(j));
        }
        ve = new RewardMockVotingEscrow();
        voter = new RewardMockVoter(address(ve));

        address[] memory feesRewards = new address[](3);
        feesRewards[0] = address(tokens[0]);
        feesRewards[1] = address(tokens[1]);
        feesRewards[2] = address(tokens[2]);
        fees = new FeesVotingReward(FORWARDER, address(voter), feesRewards);

        address[] memory bribeRewards = new address[](1);
        bribeRewards[0] = address(tokens[0]);
        bribe = new BribeVotingReward(FORWARDER, address(voter), bribeRewards);

        voter.setGaugeToFees(GAUGE, address(fees));
        // t0..t3 are whitelisted (t3 is whitelisted but not a fees reward); t4 is never whitelisted.
        for (uint256 j = 0; j < 4; j++) {
            voter.setWhitelisted(address(tokens[j]), true);
        }
    }

    /// Time moves by: the same second, the last second of the epoch, the first second of the next
    /// epoch, a random offset within a week, or one to two weeks.
    function advance() internal {
        uint256 nextEpoch = clock - (clock % WEEK) + WEEK;
        uint256 pick = rand() % 100;
        uint256 d;
        if (pick < 8) d = 0;
        else if (pick < 28) d = nextEpoch - 1 - clock;
        else if (pick < 40) d = nextEpoch - clock;
        else if (pick < 85) d = rand() % WEEK;
        else d = WEEK + rand() % WEEK;
        clock += d;
        vm.warp(clock);
    }

    function step(string memory f) internal {
        advance();
        bool isFees = rand() % 2 == 0;
        IReward r = rewardOf(isFees);
        uint256 pick = rand() % 100;
        if (pick < 4) readRound(f, r, isFees);
        else if (pick < 28) doDeposit(f, r, isFees);
        else if (pick < 44) doWithdraw(f, r, isFees);
        else if (pick < 72) doNotify(f, r, isFees);
        else doGetReward(f, r, isFees);
    }

    function doDeposit(string memory f, IReward r, bool isFees) internal {
        uint256 tid = 1 + rand() % NID;
        uint256 amount = rand() % (uint256(10) ** (rand() % 22));
        bool auth = rand() % 10 != 0;
        string memory inp = string.concat(
            "deposit|", tag(isFees), "|", s(clock), "|", s(tid), "|", s(amount), "|", b(auth)
        );
        vm.prank(auth ? address(voter) : OTHER);
        try r._deposit(amount, tid) {
            logResult(f, inp, r, "-", "");
        } catch (bytes memory reason) {
            logResult(f, inp, r, "", reasonOf(reason));
        }
    }

    /// Writes one state-changing line: `ok|payload|state` on success, `err|reason|state` on revert.
    /// `payload` is used only when `reason` is empty.
    function logResult(string memory f, string memory inp, IReward r, string memory payload, string memory reason)
        internal
    {
        if (bytes(reason).length == 0) {
            vm.writeLine(f, string.concat(inp, "|ok|", payload, "|", state(r, clock)));
        } else {
            vm.writeLine(f, string.concat(inp, "|err|", reason, "|", state(r, clock)));
        }
    }

    function doWithdraw(string memory f, IReward r, bool isFees) internal {
        uint256 tid = 1 + rand() % NID;
        uint256 bal = r.balanceOf(tid);
        // A quarter of the withdrawals exceed the balance and revert with an arithmetic panic.
        uint256 amount = rand() % 4 == 0 ? bal + 1 + rand() % 1e18 : rand() % (bal + 1);
        bool auth = rand() % 10 != 0;
        string memory inp = string.concat(
            "withdraw|", tag(isFees), "|", s(clock), "|", s(tid), "|", s(amount), "|", b(auth)
        );
        vm.prank(auth ? address(voter) : OTHER);
        try r._withdraw(amount, tid) {
            logResult(f, inp, r, "-", "");
        } catch (bytes memory reason) {
            logResult(f, inp, r, "", reasonOf(reason));
        }
    }

    function doNotify(string memory f, IReward r, bool isFees) internal {
        uint256 j = rand() % NTOK;
        uint256 p = rand() % 100;
        uint256 amount;
        if (p < 12) amount = 0;
        else if (p < 14) amount = uint256(1) << 200; // large: later earned() products overflow
        else amount = rand() % (uint256(10) ** (rand() % 24));

        address sender;
        bool flag;
        if (isFees) {
            flag = rand() % 5 != 0; // isGauge
            sender = flag ? GAUGE : OTHER;
        } else {
            flag = voter.isWhitelistedToken(address(tokens[j])); // whitelisted
            sender = BRIBER;
        }
        if (amount > 0) {
            tokens[j].mint(sender, amount);
            vm.prank(sender);
            tokens[j].approve(address(r), amount);
        }
        string memory inp = string.concat(
            "notify|", tag(isFees), "|", s(clock), "|", tokenName(j), "|", s(amount), "|", b(flag)
        );
        vm.prank(sender);
        try r.notifyRewardAmount(address(tokens[j]), amount) {
            logResult(f, inp, r, "-", "");
        } catch (bytes memory reason) {
            logResult(f, inp, r, "", reasonOf(reason));
        }
    }

    function doGetReward(string memory f, IReward r, bool isFees) internal {
        uint256 tid = 1 + rand() % NID;
        uint256 kind = rand() % 4; // 0 owner, 1 approved, 2 voter, 3 stranger
        ve.setApproved(tid, APPROVED, rand() % 2 == 0);
        address caller = kind == 0 ? owner(tid) : kind == 1 ? APPROVED : kind == 2 ? address(voter) : OTHER;
        string memory cs = kind == 0 ? "o" : kind == 1 ? "a" : kind == 2 ? "v" : "s";
        bool ap = ve.isApprovedOrOwner(caller, tid);

        (address[] memory arr, string memory names) = randomTokenList();
        uint256[5] memory before = snapshot(tid);
        string memory inp = string.concat(
            "getreward|", tag(isFees), "|", s(clock), "|", s(tid), "|", names, "|", cs, "|", b(ap)
        );
        vm.prank(caller);
        try r.getReward(tid, arr) {
            logResult(f, inp, r, payouts(tid, before), "");
        } catch (bytes memory reason) {
            logResult(f, inp, r, "", reasonOf(reason));
        }
    }

    /// 1 to 3 reward tokens, chosen at random. Duplicates are possible on purpose.
    function randomTokenList() internal returns (address[] memory arr, string memory names) {
        uint256 n = 1 + rand() % 3;
        arr = new address[](n);
        for (uint256 k = 0; k < n; k++) {
            uint256 j = rand() % NTOK;
            arr[k] = address(tokens[j]);
            names = k == 0 ? tokenName(j) : string.concat(names, ",", tokenName(j));
        }
    }

    function snapshot(uint256 tid) internal view returns (uint256[5] memory bal) {
        for (uint256 j = 0; j < NTOK; j++) {
            bal[j] = tokens[j].balanceOf(owner(tid));
        }
    }

    /// `t0=<delta>;...;t4=<delta>`: what the owner received per token since `before`.
    function payouts(uint256 tid, uint256[5] memory before) internal view returns (string memory out) {
        for (uint256 j = 0; j < NTOK; j++) {
            uint256 delta = tokens[j].balanceOf(owner(tid)) - before[j];
            out = string.concat(out, j == 0 ? "" : ";", tokenName(j), "=", s(delta));
        }
    }

    /// Reads `earned` for every (token, tokenId) at a time at or after `clock`, then returns to `clock`.
    function readRound(string memory f, IReward r, bool isFees) internal {
        uint256 pick = rand() % 4;
        uint256 t;
        if (pick == 0) t = clock;
        else if (pick == 1) t = clock + rand() % (3 * WEEK);
        else if (pick == 2) t = clock - (clock % WEEK) + WEEK - 1;
        else t = clock - (clock % WEEK) + WEEK;
        vm.warp(t);
        for (uint256 j = 0; j < NTOK; j++) {
            for (uint256 tid = 1; tid <= NID; tid++) {
                string memory inp = string.concat(
                    "earned|", tag(isFees), "|", s(t), "|", tokenName(j), "|", s(tid)
                );
                try r.earned(address(tokens[j]), tid) returns (uint256 v) {
                    vm.writeLine(f, string.concat(inp, "|ok|", s(v)));
                } catch (bytes memory reason) {
                    vm.writeLine(f, string.concat(inp, "|err|", reasonOf(reason)));
                }
            }
        }
        vm.warp(clock);
    }
}
