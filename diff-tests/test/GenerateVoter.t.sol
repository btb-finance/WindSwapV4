// SPDX-License-Identifier: GPL-3.0-or-later
pragma solidity ^0.8.19;

import {Voter} from "../windv2/contracts/Voter.sol";
import {VotingEscrow} from "../windv2/contracts/VotingEscrow.sol";
import {IVoter} from "../windv2/contracts/interfaces/IVoter.sol";
import {IVotingEscrow} from "../windv2/contracts/interfaces/IVotingEscrow.sol";
import {IReward} from "../windv2/contracts/interfaces/IReward.sol";
import {VotingRewardsFactory} from "../windv2/contracts/factories/VotingRewardsFactory.sol";
import {RewardMockToken} from "../src/rewards/RewardMockToken.sol";
import {
    VoterMockGauge,
    VoterMockGaugeFactory,
    VoterMockPool,
    VoterMockPoolFactory,
    VoterMockFactoryRegistry,
    VoterMockMinter
} from "../src/voter/VoterMockGauge.sol";

interface Vm {
    function warp(uint256 newTimestamp) external;
    function roll(uint256 newHeight) external;
    function prank(address msgSender) external;
    function load(address target, bytes32 slot) external view returns (bytes32);
    function writeFile(string calldata path, string calldata data) external;
    function writeLine(string calldata path, string calldata data) external;
    function toString(uint256 value) external pure returns (string memory);
}

/// @notice Generates differential test vectors from the real windv2 `Voter` with the real `VotingEscrow`,
/// real `FeesVotingReward` / `BribeVotingReward` (via the real `VotingRewardsFactory`) and mock gauge, pool,
/// pool factory, factory registry and minter. Output: `vectors/voter.txt`.
///
/// Every line is `op|now|block|args...|res|payload|state`, where `res` is `ok` or `err`, `payload` is the return
/// value or the revert reason (or `-`), and `state` is the full observable state after the call (see `state()`).
/// Rust replays the same ops and compares `res`, `payload` and `state` on every line.
///
/// Roles: users are `0x1000 + k` (k = 1..6), owner/governor `0x0A0A`, emergency council `0xC0C0`, stranger `0x5000`,
/// minter `0x1111` (the mock minter contract is the Voter's minter), team `777`.
/// Tokens: `t0..t4` (indices 0..4) and `WIND` (index 5, name `w`). Pools 1..4 are real mock pools over
/// (t0,t1), (t1,t2), (t2,t3), (t3,t4). Pools 5 and 6 are not pools (`isPool` false).
///
/// Run: `forge test --match-test testGenerateVoter`
contract GenerateVoterVectors {
    Vm constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

    uint256 constant OPS = 4000;
    uint256 constant T0 = 1_780_000_000;
    uint256 constant B0 = 1_000;
    uint256 constant WEEK = 1 weeks;
    uint256 constant MAXTIME = 4 * 365 * 86400;
    uint256 constant NLOCKS = 6;
    uint256 constant NPOOLS = 6;
    uint256 constant NTOK = 6;
    uint256 constant GAUGE_CAP = 6;
    address constant FORWARDER = address(0xF0F0);
    address constant OWNER = address(0x0A0A);
    address constant COUNCIL = address(0xC0C0);
    address constant STRANGER = address(0x5000);
    address constant TEAM = address(777);
    address constant BAD_FACTORY = address(0xBAD0);
    uint256 constant MINTER_ROLE = 0x1111;
    uint256 constant OWNER_ROLE = 0x0A0A;
    uint256 constant COUNCIL_ROLE = 0xC0C0;
    uint256 constant STRANGER_ROLE = 0x5000;

    uint256 seed = 0x564f5445; // "VOTE"
    uint256 clock;
    uint256 blk;
    uint256 nLocks;
    uint256 nGauges;
    uint256 nPoolsCreated;

    RewardMockToken[6] tokens;
    RewardMockToken wind;
    VotingEscrow ve;
    Voter voter;
    VoterMockFactoryRegistry registry;
    VoterMockPoolFactory poolFactory;
    VoterMockMinter minterMock;
    VoterMockGaugeFactory gaugeFactory;
    VotingRewardsFactory rewardsFactory;
    address[7] poolAddr; // index 1..6
    address[GAUGE_CAP + 1] gaugeAddr; // index 1..nGauges
    uint256[NLOCKS + 1] tokOwner; // user role of each token id (0 when not minted)
    uint256[GAUGE_CAP + 1] gaugePid; // pool id of each gauge

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

    function tname(uint256 j) internal pure returns (string memory) {
        if (j == 5) return "w";
        return string.concat("t", s(j));
    }

    function tokenOf(uint256 j) internal view returns (address) {
        return address(tokens[j]);
    }

    function addrOf(uint256 role) internal view returns (address) {
        if (role == MINTER_ROLE) return address(minterMock);
        return address(uint160(role));
    }

    function gid(address g) internal view returns (uint256) {
        for (uint256 i = 1; i <= nGauges; i++) {
            if (gaugeAddr[i] == g) return i;
        }
        return 0;
    }

    function supplyIndexOf(address g) internal view returns (uint256) {
        return uint256(vm.load(address(voter), keccak256(abi.encode(g, uint256(22)))));
    }

    /// Full observable state. Sections are `;`-separated, values `,`-separated:
    /// globals; pool weight and gauge id for pools 1..6; whitelisted token bits; per token (1..6): usedWeight,
    /// lastVoted, whitelistedNFT, voted, balanceOfNFT, votes(pool 1..6); per gauge (1..nGauges): isAlive,
    /// claimable, supplyIndex, fees totalSupply, bribe totalSupply, fees balanceOf(1..6), bribe balanceOf(1..6).
    function state() internal view returns (string memory out) {
        out = string.concat(
            s(voter.totalWeight()), ",", s(uint256(vm.load(address(voter), bytes32(uint256(21))))), ",",
            s(voter.maxVotingNum()), ",", s(voter.length())
        );
        out = string.concat(out, ";");
        for (uint256 p = 1; p <= NPOOLS; p++) {
            out = string.concat(
                out, p == 1 ? "" : ",", s(voter.weights(poolAddr[p])), ",", s(gid(voter.gauges(poolAddr[p])))
            );
        }
        out = string.concat(out, ";");
        for (uint256 j = 0; j < NTOK; j++) {
            out = string.concat(out, b(voter.isWhitelistedToken(tokenOf(j))));
        }
        for (uint256 t = 1; t <= NLOCKS; t++) {
            out = string.concat(
                out, ";", s(voter.usedWeights(t)), ",", s(voter.lastVoted(t)), ",",
                b(voter.isWhitelistedNFT(t)), ",", b(ve.voted(t)), ",", s(ve.balanceOfNFT(t))
            );
            for (uint256 p = 1; p <= NPOOLS; p++) {
                out = string.concat(out, ",", s(voter.votes(t, poolAddr[p])));
            }
        }
        for (uint256 g = 1; g <= nGauges; g++) {
            address ga = gaugeAddr[g];
            IReward f = IReward(voter.gaugeToFees(ga));
            IReward bb = IReward(voter.gaugeToBribe(ga));
            out = string.concat(
                out, ";", b(voter.isAlive(ga)), ",", s(voter.claimable(ga)), ",", s(supplyIndexOf(ga)), ",",
                s(f.totalSupply()), ",", s(bb.totalSupply())
            );
            for (uint256 t = 1; t <= NLOCKS; t++) {
                out = string.concat(out, ",", s(f.balanceOf(t)));
            }
            for (uint256 t = 1; t <= NLOCKS; t++) {
                out = string.concat(out, ",", s(bb.balanceOf(t)));
            }
        }
    }

    /// Stable reason name for a revert. Panic codes: 0x11 overflow/underflow, 0x12 division by zero, 0x32 index.
    function reasonOf(bytes memory r) internal pure returns (string memory) {
        if (r.length < 4) return "empty";
        bytes32 w;
        assembly {
            w := mload(add(r, 32))
        }
        bytes4 sel = bytes4(w);
        if (sel == IVoter.AlreadyVotedOrDeposited.selector) return "AlreadyVotedOrDeposited";
        if (sel == IVoter.DistributeWindow.selector) return "DistributeWindow";
        if (sel == IVoter.NotApprovedOrOwner.selector) return "NotApprovedOrOwner";
        if (sel == IVoter.UnequalLengths.selector) return "UnequalLengths";
        if (sel == IVoter.TooManyPools.selector) return "TooManyPools";
        if (sel == IVoter.NotWhitelistedNFT.selector) return "NotWhitelistedNFT";
        if (sel == IVoter.NotGovernor.selector) return "NotGovernor";
        if (sel == IVoter.NotEmergencyCouncil.selector) return "NotEmergencyCouncil";
        if (sel == IVoter.NotMinter.selector) return "NotMinter";
        if (sel == IVoter.MaximumVotingNumberTooLow.selector) return "MaximumVotingNumberTooLow";
        if (sel == IVoter.SameValue.selector) return "SameValue";
        if (sel == IVoter.GaugeDoesNotExist.selector) return "GaugeDoesNotExist";
        if (sel == IVoter.GaugeNotAlive.selector) return "GaugeNotAlive";
        if (sel == IVoter.NonZeroVotes.selector) return "NonZeroVotes";
        if (sel == IVoter.ZeroBalance.selector) return "ZeroBalance";
        if (sel == IVoter.FactoryPathNotApproved.selector) return "FactoryPathNotApproved";
        if (sel == IVoter.GaugeExists.selector) return "GaugeExists";
        if (sel == IVoter.NotAPool.selector) return "NotAPool";
        if (sel == IVoter.NotWhitelistedToken.selector) return "NotWhitelistedToken";
        if (sel == IVoter.GaugeAlreadyKilled.selector) return "GaugeAlreadyKilled";
        if (sel == IVoter.GaugeAlreadyRevived.selector) return "GaugeAlreadyRevived";
        if (sel == IReward.NotWhitelisted.selector) return "NotWhitelisted";
        if (sel == IReward.ZeroAmount.selector) return "ZeroAmount";
        if (sel == IReward.NotAuthorized.selector) return "NotAuthorized";
        if (sel == IReward.NotGauge.selector) return "NotGauge";
        if (sel == IReward.InvalidReward.selector) return "InvalidReward";
        if (sel == IVotingEscrow.LockExpired.selector) return "LockExpired";
        if (sel == IVotingEscrow.PermanentLock.selector) return "PermanentLock";
        if (sel == IVotingEscrow.NoLockFound.selector) return "NoLockFound";
        if (sel == IVotingEscrow.LockDurationTooLong.selector) return "LockDurationTooLong";
        if (sel == IVotingEscrow.LockDurationNotInFuture.selector) return "LockDurationNotInFuture";
        if (sel == bytes4(0x4e487b71)) {
            uint256 code;
            assembly {
                code := mload(add(r, 36))
            }
            if (code == 0x11) return "Panic17";
            if (code == 0x12) return "Panic18";
            if (code == 0x32) return "Panic50";
            return "PanicOther";
        }
        return "unknown";
    }

    function logOk(string memory f, string memory ins, string memory payload) internal {
        vm.writeLine(f, string.concat(ins, "|ok|", payload, "|", state()));
    }

    function logErr(string memory f, string memory ins, bytes memory r) internal {
        vm.writeLine(f, string.concat(ins, "|err|", reasonOf(r), "|", state()));
    }

    function testGenerateVoter() public {
        string memory f = "vectors/voter.txt";
        vm.writeFile(f, "");
        deployAll();
        vm.writeLine(f, string.concat("init|", s(clock), "|", s(blk), "|ok|-|", state()));
        for (uint256 i = 0; i < OPS; i++) {
            // Each step runs in its own call frame so its string memory is released before the next one.
            this.stepExternal(f);
        }
    }

    function stepExternal(string memory f) external {
        require(msg.sender == address(this), "self only");
        step(f);
    }

    function deployAll() internal {
        clock = T0;
        blk = B0;
        vm.warp(clock);
        vm.roll(blk);
        for (uint256 j = 0; j < 5; j++) {
            tokens[j] = new RewardMockToken(tname(j));
        }
        wind = new RewardMockToken("WIND");
        tokens[5] = wind;

        ve = new VotingEscrow(FORWARDER, address(wind), address(0), TEAM);
        registry = new VoterMockFactoryRegistry();
        poolFactory = new VoterMockPoolFactory();
        minterMock = new VoterMockMinter();
        gaugeFactory = new VoterMockGaugeFactory();
        rewardsFactory = new VotingRewardsFactory();
        registry.setPoolFactory(address(poolFactory), address(rewardsFactory), address(gaugeFactory));

        voter = new Voter(FORWARDER, address(ve), address(registry), OWNER);

        for (uint256 i = 1; i <= 4; i++) {
            poolAddr[i] = address(new VoterMockPool(tokenOf(i - 1), tokenOf(i)));
            poolFactory.setPool(poolAddr[i], true);
        }
        poolAddr[5] = address(0x9005);
        poolAddr[6] = address(0x9006);

        vm.prank(TEAM);
        ve.setVoterAndDistributor(address(voter), address(minterMock));

        address[] memory initTokens = new address[](3);
        initTokens[0] = tokenOf(0);
        initTokens[1] = tokenOf(1);
        initTokens[2] = address(wind);
        vm.prank(OWNER);
        voter.initialize(initTokens, address(minterMock));
        vm.prank(OWNER);
        voter.setEmergencyCouncil(COUNCIL);

        for (uint256 k = 1; k <= 6; k++) {
            vm.prank(addrOf(0x1000 + k));
            wind.approve(address(ve), type(uint256).max);
        }
    }

    function advance() internal {
        uint256 ep = clock - (clock % WEEK);
        uint256 next = ep + WEEK;
        uint256 pick = rand() % 100;
        uint256 target;
        if (pick < 10) {
            target = clock;
        } else if (pick < 18) {
            target = next - 1 - rand() % 3600; // last hour: vote window closed
        } else if (pick < 26) {
            target = next + rand() % 3600; // first hour: distribute window
        } else if (pick < 86) {
            target = clock + rand() % (6 hours);
        } else if (pick < 98) {
            target = clock + rand() % (2 days);
        } else {
            target = clock + rand() % WEEK;
        }
        if (target > clock) {
            clock = target;
        }
        vm.warp(clock);
    }

    function step(string memory f) internal {
        advance();
        blk += 1;
        vm.roll(blk);
        uint256 pick = rand() % 100;
        if (pick < 6) {
            if (nLocks < NLOCKS) doLock(f);
            else doVote(f);
        } else if (pick < 29) doVote(f);
        else if (pick < 33) doReset(f);
        else if (pick < 37) doPoke(f);
        else if (pick < 40) doCreateGauge(f);
        else if (pick < 46) doNotify(f);
        else if (pick < 52) doBribeNotify(f);
        else if (pick < 62) doFeeNotify(f);
        else if (pick < 68) doDistribute(f);
        else if (pick < 71) doUpdateFor(f);
        else if (pick < 73) doKill(f);
        else if (pick < 75) doWhitelistToken(f);
        else if (pick < 76) doWhitelistNft(f);
        else if (pick < 77) doMaxVoting(f);
        else if (pick < 90) doClaim(f);
        else if (pick < 91) doPermanent(f);
        else if (pick < 95) doExtend(f);
        else doEarned(f);
    }

    function doLock(string memory f) internal {
        uint256 k = 1 + rand() % 6;
        uint256 role = 0x1000 + k;
        uint256 p = rand() % 100;
        uint256 value = p < 3 ? 0 : 1 + rand() % 1e21;
        uint256 p2 = rand() % 100;
        uint256 dur;
        if (p2 < 3) dur = 0;
        else if (p2 < 6) dur = MAXTIME + 1 + rand() % 1e6;
        else if (p2 < 12) dur = WEEK * (1 + rand() % 52) + rand() % WEEK;
        else dur = MAXTIME - rand() % WEEK; // near the maximum: keeps voting power alive
        string memory ins = string.concat("lock|", s(clock), "|", s(blk), "|", s(role), "|", s(value), "|", s(dur));
        wind.mint(addrOf(role), value);
        vm.prank(addrOf(role));
        try ve.createLock(value, dur) returns (uint256 id) {
            nLocks += 1;
            tokOwner[id] = k;
            logOk(f, ins, s(id));
        } catch (bytes memory r) {
            logErr(f, ins, r);
        }
    }

    function senderOf(uint256 t) internal returns (uint256) {
        uint256 p = rand() % 100;
        if (p < 90 && tokOwner[t] != 0) return 0x1000 + tokOwner[t];
        if (p < 85) return STRANGER_ROLE;
        return 0x1000 + 1 + rand() % 6;
    }

    function doVote(string memory f) internal {
        uint256 t = 1 + rand() % NLOCKS;
        uint256 who = senderOf(t);
        uint256 np;
        uint256 pick = rand() % 100;
        if (pick < 2) np = 0;
        else if (pick < 3) np = voter.maxVotingNum() + 1;
        else np = 1 + rand() % 4;
        address[] memory pools = new address[](np);
        uint256[] memory weights = new uint256[](np);
        string memory pidStr;
        string memory wStr;
        uint256 vstart = rand() % NPOOLS;
        for (uint256 i = 0; i < np; i++) {
            // Mostly distinct pools (consecutive from a random start); sometimes random, so duplicates occur.
            uint256 pid = rand() % 100 < 90 ? 1 + (vstart + i) % NPOOLS : 1 + rand() % NPOOLS;
            pools[i] = poolAddr[pid];
            uint256 wp = rand() % 100;
            uint256 w;
            if (wp < 2) w = 0;
            else if (wp < 94) w = 1 + rand() % 1000;
            else if (wp < 98) w = 1e6 + rand() % 1e12;
            else if (wp < 99) w = 1 << 200;
            else w = 1 + rand() % 1000;
            weights[i] = w;
            pidStr = string.concat(pidStr, i == 0 ? "" : ",", s(pid));
            wStr = string.concat(wStr, i == 0 ? "" : ",", s(w));
        }
        string memory ins = string.concat(
            "vote|", s(clock), "|", s(blk), "|", s(t), "|", s(who), "|", pidStr, "|", wStr
        );
        vm.prank(addrOf(who));
        try voter.vote(t, pools, weights) {
            logOk(f, ins, "-");
        } catch (bytes memory r) {
            logErr(f, ins, r);
        }
    }

    function doReset(string memory f) internal {
        uint256 t = 1 + rand() % NLOCKS;
        uint256 who = senderOf(t);
        string memory ins = string.concat("reset|", s(clock), "|", s(blk), "|", s(t), "|", s(who));
        vm.prank(addrOf(who));
        try voter.reset(t) {
            logOk(f, ins, "-");
        } catch (bytes memory r) {
            logErr(f, ins, r);
        }
    }

    function doPoke(string memory f) internal {
        uint256 t = 1 + rand() % NLOCKS;
        string memory ins = string.concat("poke|", s(clock), "|", s(blk), "|", s(t));
        try voter.poke(t) {
            logOk(f, ins, "-");
        } catch (bytes memory r) {
            logErr(f, ins, r);
        }
    }

    function doCreateGauge(string memory f) internal {
        uint256 caller = rand() % 2 == 0 ? OWNER_ROLE : STRANGER_ROLE;
        uint256 pid = 1 + rand() % NPOOLS;
        if (rand() % 100 < 80) {
            // Prefer a pool without a gauge yet, so that most attempts succeed.
            for (uint256 i = 0; i < NPOOLS; i++) {
                if (voter.gauges(poolAddr[pid]) == address(0)) break;
                pid = 1 + pid % NPOOLS;
            }
        }
        bool approved = rand() % 100 < 85;
        address pf = approved ? address(poolFactory) : BAD_FACTORY;
        string memory ins = string.concat(
            "creategauge|", s(clock), "|", s(blk), "|", s(caller), "|", s(pid), "|", b(approved)
        );
        vm.prank(addrOf(caller));
        try voter.createGauge(pf, poolAddr[pid]) returns (address g) {
            nGauges += 1;
            gaugeAddr[nGauges] = g;
            gaugePid[nGauges] = pid;
            logOk(f, ins, s(nGauges));
        } catch (bytes memory r) {
            logErr(f, ins, r);
        }
    }

    function doNotify(string memory f) internal {
        uint256 amount;
        uint256 p = rand() % 100;
        if (p < 10) amount = 0;
        else if (p < 60) amount = rand() % 1e21;
        else if (p < 99) amount = rand() % 1e30;
        else amount = 1 << 200;
        bool fromMinter = rand() % 100 < 95;
        uint256 caller = fromMinter ? MINTER_ROLE : STRANGER_ROLE;
        string memory ins = string.concat("notify|", s(clock), "|", s(blk), "|", s(caller), "|", s(amount));
        if (fromMinter) {
            wind.mint(address(minterMock), amount);
            vm.prank(address(minterMock));
            wind.approve(address(voter), amount);
        }
        vm.prank(addrOf(caller));
        try voter.notifyRewardAmount(amount) {
            logOk(f, ins, "-");
        } catch (bytes memory r) {
            logErr(f, ins, r);
        }
    }

    function doBribeNotify(string memory f) internal {
        if (nGauges == 0) return;
        uint256 g = 1 + rand() % nGauges;
        uint256 j = rand() % NTOK;
        uint256 amount = rand() % 100 < 5 ? 0 : rand() % 1e20;
        IReward r = IReward(voter.gaugeToBribe(gaugeAddr[g]));
        string memory ins = string.concat(
            "bnotify|", s(clock), "|", s(blk), "|", s(g), "|", tname(j), "|", s(amount)
        );
        tokens[j].mint(addrOf(STRANGER_ROLE + 1), amount);
        vm.prank(addrOf(STRANGER_ROLE + 1));
        tokens[j].approve(address(r), amount);
        vm.prank(addrOf(STRANGER_ROLE + 1));
        try r.notifyRewardAmount(address(tokens[j]), amount) {
            logOk(f, ins, "-");
        } catch (bytes memory er) {
            logErr(f, ins, er);
        }
    }

    function doDistribute(string memory f) internal {
        for (uint256 g = 1; g <= nGauges; g++) {
            uint256 left;
            uint256 lp = rand() % 100;
            if (lp < 30) left = 0;
            else if (lp < 60) left = rand() % 1e21;
            else left = rand() % 1e30;
            bool fail = rand() % 100 < 15;
            VoterMockGauge(gaugeAddr[g]).setLeft(left);
            VoterMockGauge(gaugeAddr[g]).setFail(fail);
            logOk(f, string.concat("gcfg|", s(clock), "|", s(blk), "|", s(g), "|", s(left), "|", b(fail)), "-");
        }
        uint256 kind = rand() % 3;
        if (nGauges == 0) kind = 0;
        uint256 before = gaugeWind();
        if (kind == 0) {
            uint256 start = rand() % (voter.length() + 2);
            uint256 finish = start + rand() % 3;
            string memory ins = string.concat("distr|", s(clock), "|", s(blk), "|r|", s(start), "|", s(finish));
            try voter.distribute(start, finish) {
                logOk(f, ins, s(gaugeWind() - before));
            } catch (bytes memory r) {
                logErr(f, ins, r);
            }
        } else if (kind == 1) {
            uint256 n = rand() % 4;
            address[] memory gs = new address[](n);
            string memory ids;
            for (uint256 i = 0; i < n; i++) {
                uint256 g = 1 + rand() % nGauges;
                gs[i] = gaugeAddr[g];
                ids = string.concat(ids, i == 0 ? "" : ",", s(g));
            }
            string memory ins = string.concat("distl|", s(clock), "|", s(blk), "|", ids);
            try voter.distribute(gs) {
                logOk(f, ins, s(gaugeWind() - before));
            } catch (bytes memory r) {
                logErr(f, ins, r);
            }
        } else {
            uint256 g = 1 + rand() % nGauges;
            string memory ins = string.concat("distg|", s(clock), "|", s(blk), "|", s(g));
            try voter.distributeForGauge(gaugeAddr[g]) {
                logOk(f, ins, s(gaugeWind() - before));
            } catch (bytes memory r) {
                logErr(f, ins, r);
            }
        }
    }

    /// Total WIND held by the gauges: what `_distribute` transfers in (the distribute payload).
    function gaugeWind() internal view returns (uint256 total) {
        for (uint256 g = 1; g <= nGauges; g++) {
            total += wind.balanceOf(gaugeAddr[g]);
        }
    }

    /// Fee notification: as the gauge (its `notifyFees`, after minting the token to it) or as a stranger (which
    /// reverts with `NotGauge`). Tokens are the gauge's pool tokens half the time, else any token (`InvalidReward`).
    function doFeeNotify(string memory f) internal {
        if (nGauges == 0) return;
        uint256 g = 1 + rand() % nGauges;
        uint256 j;
        uint256 pid = gaugePid[g];
        if (rand() % 2 == 0 && pid >= 1 && pid <= 4) j = rand() % 2 == 0 ? pid - 1 : pid;
        else j = rand() % NTOK;
        uint256 amount = rand() % 100 < 5 ? 0 : rand() % 1e20;
        bool viaGauge = rand() % 100 < 85;
        address fees = voter.gaugeToFees(gaugeAddr[g]);
        string memory ins = string.concat(
            "fnotify|", s(clock), "|", s(blk), "|", s(g), "|", tname(j), "|", s(amount), "|", viaGauge ? "1" : "0"
        );
        if (viaGauge) {
            tokens[j].mint(gaugeAddr[g], amount);
            try VoterMockGauge(gaugeAddr[g]).notifyFees(address(tokens[j]), amount) {
                logOk(f, ins, "-");
            } catch (bytes memory r) {
                logErr(f, ins, r);
            }
        } else {
            vm.prank(addrOf(STRANGER_ROLE));
            try IReward(fees).notifyRewardAmount(address(tokens[j]), amount) {
                logOk(f, ins, "-");
            } catch (bytes memory r) {
                logErr(f, ins, r);
            }
        }
    }

    /// `lockPermanent` by the owner (or a stranger, which reverts with `NotApprovedOrOwner`).
    function doPermanent(string memory f) internal {
        if (nLocks == 0) return;
        uint256 t = 1 + rand() % nLocks;
        uint256 who = rand() % 100 < 90 ? 0x1000 + tokOwner[t] : STRANGER_ROLE;
        string memory ins = string.concat("perm|", s(clock), "|", s(blk), "|", s(t), "|", s(who));
        vm.prank(addrOf(who));
        try ve.lockPermanent(t) {
            logOk(f, ins, "-");
        } catch (bytes memory r) {
            logErr(f, ins, r);
        }
    }

    /// `increaseUnlockTime` to the maximum lock (keeps voting power alive), or a short duration that is not in
    /// the future (reverts with `LockDurationNotInFuture`).
    function doExtend(string memory f) internal {
        if (nLocks == 0) return;
        uint256 t = 1 + rand() % nLocks;
        uint256 who = rand() % 100 < 90 ? 0x1000 + tokOwner[t] : STRANGER_ROLE;
        uint256 dur = rand() % 100 < 90 ? MAXTIME - rand() % WEEK : WEEK * (1 + rand() % 8);
        string memory ins = string.concat(
            "extend|", s(clock), "|", s(blk), "|", s(t), "|", s(who), "|", s(dur)
        );
        vm.prank(addrOf(who));
        try ve.increaseUnlockTime(t, dur) {
            logOk(f, ins, "-");
        } catch (bytes memory r) {
            logErr(f, ins, r);
        }
    }

    function doUpdateFor(string memory f) internal {
        uint256 kind = rand() % 3;
        uint256 before = wind.balanceOf(address(minterMock));
        if (kind == 0) {
            uint256 n = 1 + rand() % 3;
            address[] memory gs = new address[](n);
            string memory ids;
            for (uint256 i = 0; i < n; i++) {
                uint256 g = nGauges == 0 ? 0 : rand() % (nGauges + 1);
                gs[i] = g == 0 ? address(0) : gaugeAddr[g];
                ids = string.concat(ids, i == 0 ? "" : ",", s(g));
            }
            string memory ins = string.concat("upl|", s(clock), "|", s(blk), "|", ids);
            try voter.updateFor(gs) {
                logOk(f, ins, s(wind.balanceOf(address(minterMock)) - before));
            } catch (bytes memory r) {
                logErr(f, ins, r);
            }
        } else if (kind == 1) {
            uint256 start = rand() % (voter.length() + 2);
            uint256 end = start + rand() % 3;
            string memory ins = string.concat("upr|", s(clock), "|", s(blk), "|", s(start), "|", s(end));
            try voter.updateFor(start, end) {
                logOk(f, ins, s(wind.balanceOf(address(minterMock)) - before));
            } catch (bytes memory r) {
                logErr(f, ins, r);
            }
        } else {
            uint256 g = nGauges == 0 ? 0 : rand() % (nGauges + 1);
            address ga = g == 0 ? address(0) : gaugeAddr[g];
            string memory ins = string.concat("upg|", s(clock), "|", s(blk), "|", s(g));
            try voter.updateFor(ga) {
                logOk(f, ins, s(wind.balanceOf(address(minterMock)) - before));
            } catch (bytes memory r) {
                logErr(f, ins, r);
            }
        }
    }

    function doKill(string memory f) internal {
        if (nGauges == 0) return;
        uint256 g = 1 + rand() % nGauges;
        uint256 caller = rand() % 100 < 90 ? COUNCIL_ROLE : STRANGER_ROLE;
        bool kill = rand() % 10 < 1;
        uint256 before = wind.balanceOf(address(minterMock));
        if (kill) {
            string memory ins = string.concat("kill|", s(clock), "|", s(blk), "|", s(g), "|", s(caller));
            vm.prank(addrOf(caller));
            try voter.killGauge(gaugeAddr[g]) {
                logOk(f, ins, s(wind.balanceOf(address(minterMock)) - before));
            } catch (bytes memory r) {
                logErr(f, ins, r);
            }
        } else {
            string memory ins = string.concat("revive|", s(clock), "|", s(blk), "|", s(g), "|", s(caller));
            vm.prank(addrOf(caller));
            try voter.reviveGauge(gaugeAddr[g]) {
                logOk(f, ins, "-");
            } catch (bytes memory r) {
                logErr(f, ins, r);
            }
        }
    }

    function doWhitelistToken(string memory f) internal {
        uint256 j = rand() % NTOK;
        bool flag = rand() % 2 == 0;
        uint256 caller = rand() % 100 < 90 ? OWNER_ROLE : STRANGER_ROLE;
        string memory ins = string.concat(
            "wltoken|", s(clock), "|", s(blk), "|", tname(j), "|", b(flag), "|", s(caller)
        );
        vm.prank(addrOf(caller));
        try voter.whitelistToken(tokenOf(j), flag) {
            logOk(f, ins, "-");
        } catch (bytes memory r) {
            logErr(f, ins, r);
        }
    }

    function doWhitelistNft(string memory f) internal {
        uint256 t = 1 + rand() % NLOCKS;
        bool flag = rand() % 2 == 0;
        uint256 caller = rand() % 100 < 90 ? OWNER_ROLE : STRANGER_ROLE;
        string memory ins = string.concat(
            "wlnft|", s(clock), "|", s(blk), "|", s(t), "|", b(flag), "|", s(caller)
        );
        vm.prank(addrOf(caller));
        try voter.whitelistNFT(t, flag) {
            logOk(f, ins, "-");
        } catch (bytes memory r) {
            logErr(f, ins, r);
        }
    }

    function doMaxVoting(string memory f) internal {
        uint256 pick = rand() % 6;
        uint256 v = pick == 0 ? 5 : pick == 1 ? 9 : pick == 2 ? 10 : pick == 3 ? 12 : pick == 4 ? 30 : 40;
        uint256 caller = rand() % 100 < 90 ? OWNER_ROLE : STRANGER_ROLE;
        string memory ins = string.concat("maxvote|", s(clock), "|", s(blk), "|", s(v), "|", s(caller));
        vm.prank(addrOf(caller));
        try voter.setMaxVotingNum(v) {
            logOk(f, ins, "-");
        } catch (bytes memory r) {
            logErr(f, ins, r);
        }
    }

    function doClaim(string memory f) internal {
        if (nGauges == 0) return;
        uint256 t = 1 + rand() % NLOCKS;
        if (tokOwner[t] == 0) return;
        bool fees = rand() % 2 == 0;
        uint256 who;
        uint256 wp = rand() % 100;
        if (wp < 60) who = 0x1000 + tokOwner[t];
        else if (wp < 80) who = STRANGER_ROLE;
        else who = 0x1000 + 1 + rand() % 6;

        uint256 ne = 1 + rand() % 3;
        address[] memory rs = new address[](ne);
        address[][] memory tks = new address[][](ne);
        string memory entries;
        for (uint256 e = 0; e < ne; e++) {
            uint256 g = pickClaimGauge(t);
            uint256 pid = gaugePid[g];
            uint256 nt = 1 + rand() % 3;
            address[] memory tl = new address[](nt);
            string memory names;
            for (uint256 i = 0; i < nt; i++) {
                // Mostly the gauge's own pool tokens, which are the reward tokens of its fees and bribe contracts.
                uint256 j = rand() % 100 < 70 && pid >= 1 && pid <= 4 ? (rand() % 2 == 0 ? pid - 1 : pid) : rand() % NTOK;
                tl[i] = tokenOf(j);
                names = string.concat(names, i == 0 ? "" : ",", tname(j));
            }
            rs[e] = fees ? voter.gaugeToFees(gaugeAddr[g]) : voter.gaugeToBribe(gaugeAddr[g]);
            tks[e] = tl;
            entries = string.concat(entries, e == 0 ? "" : ";", s(g), ":", names);
        }
        address owner_ = addrOf(0x1000 + tokOwner[t]);
        uint256[NTOK] memory before;
        for (uint256 j = 0; j < NTOK; j++) {
            before[j] = tokens[j].balanceOf(owner_);
        }
        string memory ins = string.concat(
            "claim|", s(clock), "|", s(blk), "|", fees ? "F" : "B", "|", s(t), "|", s(who), "|", entries
        );
        vm.prank(addrOf(who));
        if (fees) {
            try voter.claimFees(rs, tks, t) {
                logOk(f, ins, payoutStr(owner_, before));
            } catch (bytes memory r) {
                logErr(f, ins, r);
            }
        } else {
            try voter.claimBribes(rs, tks, t) {
                logOk(f, ins, payoutStr(owner_, before));
            } catch (bytes memory r) {
                logErr(f, ins, r);
            }
        }
    }

    /// A gauge the token currently votes for (most of the time), else any gauge.
    function pickClaimGauge(uint256 t) internal returns (uint256) {
        uint256[NPOOLS] memory cands;
        uint256 n;
        for (uint256 p = 1; p <= NPOOLS; p++) {
            uint256 g = gid(voter.gauges(poolAddr[p]));
            if (g != 0 && voter.votes(t, poolAddr[p]) > 0) cands[n++] = g;
        }
        if (n > 0 && rand() % 100 < 80) return cands[rand() % n];
        return 1 + rand() % nGauges;
    }

    function payoutStr(address owner_, uint256[NTOK] memory before) internal view returns (string memory out) {
        for (uint256 j = 0; j < NTOK; j++) {
            out = string.concat(out, j == 0 ? "" : ",", s(tokens[j].balanceOf(owner_) - before[j]));
        }
    }

    function doEarned(string memory f) internal {
        if (nGauges == 0) return;
        uint256 g = 1 + rand() % nGauges;
        bool fees = rand() % 2 == 0;
        uint256 j = rand() % NTOK;
        uint256 t = 1 + rand() % NLOCKS;
        uint256 at;
        uint256 ap = rand() % 100;
        if (ap < 50) at = clock;
        else if (ap < 80) at = clock + rand() % (3 * WEEK);
        else at = clock - (clock % WEEK) + WEEK;
        IReward r = IReward(fees ? voter.gaugeToFees(gaugeAddr[g]) : voter.gaugeToBribe(gaugeAddr[g]));
        string memory ins = string.concat(
            "earned|", s(clock), "|", s(blk), "|", fees ? "F" : "B", "|", s(g), "|", tname(j), "|", s(t), "|", s(at)
        );
        vm.warp(at);
        try r.earned(address(tokens[j]), t) returns (uint256 v) {
            vm.warp(clock);
            logOk(f, ins, s(v));
        } catch (bytes memory er) {
            vm.warp(clock);
            logErr(f, ins, er);
        }
    }
}
