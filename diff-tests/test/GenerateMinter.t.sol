// SPDX-License-Identifier: GPL-3.0-or-later
pragma solidity ^0.8.19;

import {Minter} from "../windv2/contracts/Minter.sol";
import {IMinter} from "../windv2/contracts/interfaces/IMinter.sol";
import {MockWind} from "../src/minter/MockWind.sol";
import {MockVotingEscrow} from "../src/minter/MockVotingEscrow.sol";
import {MockVoter} from "../src/minter/MockVoter.sol";
import {MockEpochGovernor} from "../src/minter/MockEpochGovernor.sol";
import {MockRewardsDistributor} from "../src/minter/MockRewardsDistributor.sol";

interface Vm {
    function warp(uint256 newTimestamp) external;
    function prank(address msgSender) external;
    function writeFile(string calldata path, string calldata data) external;
    function writeLine(string calldata path, string calldata data) external;
    function toString(uint256 value) external pure returns (string memory);
}

/// @notice Generates differential test vectors from the real windv2 `Minter` (deployed with mocks).
/// Output: `vectors/minter.txt`, one line per call. Every line ends with the state (or, for
/// `calc`, the call's inputs and outputs). Formats:
///  - `init|now|state`
///  - `update|now|wind|minterBal|ve|noop|state`
///  - `update|now|wind|minterBal|ve|ok|emission|growth|team|mintRequired|state`
///  - `update|now|wind|minterBal|ve|err|reason|state`
///  - `nudge|govState|ok|state` / `nudge|govState|err|reason|state`
///  - `teamRate|rate|ok|state` / `teamRate|rate|err|reason|state`
///  - `calc|minted|ve|wind|ok|growth` / `calc|minted|ve|wind|err|reason`
/// `state` is `weekly,tailEmissionRate,teamRate,activePeriod,epochCount,nudgedThisPeriod`.
/// `reason` is one of `TailEmissionsInactive`, `AlreadyNudged`, `RateTooHigh`, `Panic17` (arithmetic
/// overflow or underflow), `Panic18` (division by zero).
/// The generator also checks, inline, the token conservation of every successful update.
/// Run: `forge test --match-test testGenerateMinter`
contract GenerateMinterVectors {
    Vm constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

    uint256 constant STEPS = 320;
    uint256 constant T0 = 1_780_000_000;
    uint256 constant WEEK = 1 weeks;
    uint256 constant INITIAL_SUPPLY = 30_000_000 ether;
    address constant TEAM = address(0x7EA4);
    address constant HOLDER = address(0xA11CE);

    uint256 seed = 0x4d494e54; // "MINT"
    uint256 clock;

    MockWind wind;
    MockVotingEscrow ve;
    MockVoter voter;
    MockEpochGovernor gov;
    MockRewardsDistributor dist;
    Minter minter;

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

    /// Full observable state of the Minter.
    function state() internal view returns (string memory) {
        uint256 ap = minter.activePeriod();
        return string.concat(
            s(minter.weekly()),
            ",",
            s(minter.tailEmissionRate()),
            ",",
            s(minter.teamRate()),
            ",",
            s(ap),
            ",",
            s(minter.epochCount()),
            ",",
            b(minter.proposals(ap))
        );
    }

    /// Maps the revert data of a failed Minter call to a stable reason name.
    function reasonOf(bytes memory r) internal pure returns (string memory) {
        if (r.length < 4) return "empty";
        bytes32 w;
        assembly {
            w := mload(add(r, 32))
        }
        bytes4 sel = bytes4(w);
        if (sel == IMinter.TailEmissionsInactive.selector) return "TailEmissionsInactive";
        if (sel == IMinter.AlreadyNudged.selector) return "AlreadyNudged";
        if (sel == IMinter.RateTooHigh.selector) return "RateTooHigh";
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

    function testGenerateMinter() public {
        string memory f = "vectors/minter.txt";
        vm.writeFile(f, "");
        deployAll();
        vm.writeLine(f, string.concat("init|", s(clock), "|", state()));
        for (uint256 i = 0; i < STEPS; i++) {
            step(f);
        }
    }

    function deployAll() internal {
        clock = T0;
        vm.warp(clock);
        wind = new MockWind();
        ve = new MockVotingEscrow(address(wind));
        voter = new MockVoter(address(wind));
        gov = new MockEpochGovernor();
        dist = new MockRewardsDistributor();
        voter.setEpochGovernor(address(gov));
        minter = new Minter(address(voter), address(ve), address(dist), TEAM);
        wind.setMinter(address(minter));
        voter.setMinter(address(minter));
        wind.testMint(HOLDER, INITIAL_SUPPLY);
    }

    function step(string memory f) internal {
        // Time: mostly one week, sometimes less (no-op), sometimes several weeks.
        uint256 pick = rand() % 100;
        uint256 d;
        if (pick < 15) d = rand() % WEEK + 1;
        else if (pick < 90) d = WEEK + rand() % (WEEK / 2);
        else d = 2 * WEEK + rand() % WEEK;
        clock += d;
        vm.warp(clock);

        // ve total supply in [0, wind supply]; rarely above it (the Solidity reverts on underflow).
        uint256 w = wind.totalSupply();
        if (rand() % 25 == 0) ve.setTotalSupply(w + 1 + rand() % 1e20);
        else ve.setTotalSupply(rand() % (w + 1));

        // Sometimes move supply around: mint to the holder, or to the minter's own balance.
        if (rand() % 5 == 0) {
            address to = rand() % 3 == 0 ? address(minter) : HOLDER;
            wind.testMint(to, rand() % (w / 10 + 1));
        }

        doUpdate(f);
        if (rand() % 100 < 15) doCalc(f);
        if (rand() % 100 < 30) doNudges(f);
        if (rand() % 100 < 10) doTeamRate(f);
    }

    function doUpdate(string memory f) internal {
        uint256 nowTs = clock;
        uint256 w = wind.totalSupply();
        uint256 bal = wind.balanceOf(address(minter));
        uint256 v = ve.totalSupplyValue();
        uint256 teamBefore = wind.balanceOf(TEAM);
        uint256 distBefore = wind.balanceOf(address(dist));
        uint256 epochBefore = minter.epochCount();
        string memory inp = string.concat("update|", s(nowTs), "|", s(w), "|", s(bal), "|", s(v));

        try minter.updatePeriod() returns (uint256) {
            if (minter.epochCount() == epochBefore) {
                vm.writeLine(f, string.concat(inp, "|noop|", state()));
                return;
            }
            uint256 emission = voter.lastNotify();
            uint256 growth = wind.balanceOf(address(dist)) - distBefore;
            uint256 team = wind.balanceOf(TEAM) - teamBefore;
            uint256 mintReq = wind.totalSupply() - w;
            // Conservation: the minter's balance after = before + minted - (emission + growth + team).
            require(
                wind.balanceOf(address(minter)) + emission + growth + team == bal + mintReq, "conservation"
            );
            vm.writeLine(
                f,
                string.concat(
                    inp, "|ok|", s(emission), "|", s(growth), "|", s(team), "|", s(mintReq), "|", state()
                )
            );
        } catch (bytes memory reason) {
            vm.writeLine(f, string.concat(inp, "|err|", reasonOf(reason), "|", state()));
        }
    }

    function doCalc(string memory f) internal {
        uint256 w = wind.totalSupply();
        uint256 minted = rand() % (2 * w + 1);
        uint256 v = rand() % 4 == 0 ? w + 1 : rand() % (w + 1);
        ve.setTotalSupply(v);
        string memory inp = string.concat("calc|", s(minted), "|", s(v), "|", s(w));
        try minter.calculateGrowth(minted) returns (uint256 growth) {
            vm.writeLine(f, string.concat(inp, "|ok|", s(growth)));
        } catch (bytes memory reason) {
            vm.writeLine(f, string.concat(inp, "|err|", reasonOf(reason)));
        }
    }

    /// Nudges come only from the epoch governor. Several per period, with states that include
    /// Succeeded, Expired and the "not succeeded" states, so both branches and `AlreadyNudged` occur.
    function doNudges(string memory f) internal {
        uint8 st = pickState();
        gov.setState(st);
        nudgeOnce(f, st);
        if (rand() % 3 == 0) {
            uint8 again = pickState();
            gov.setState(again);
            nudgeOnce(f, again);
        }
    }

    function pickState() internal returns (uint8) {
        uint256 p = rand() % 10;
        if (p < 4) return 4; // Succeeded
        if (p < 6) return 6; // Expired
        return uint8(rand() % 8);
    }

    function nudgeOnce(string memory f, uint8 st) internal {
        string memory inp = string.concat("nudge|", s(st));
        try gov.callNudge(address(minter)) {
            vm.writeLine(f, string.concat(inp, "|ok|", state()));
        } catch (bytes memory reason) {
            vm.writeLine(f, string.concat(inp, "|err|", reasonOf(reason), "|", state()));
        }
    }

    /// Team rates above `MAXIMUM_TEAM_RATE` (500) revert with `RateTooHigh`.
    function doTeamRate(string memory f) internal {
        uint256 rate = rand() % 600;
        string memory inp = string.concat("teamRate|", s(rate));
        vm.prank(TEAM);
        try minter.setTeamRate(rate) {
            vm.writeLine(f, string.concat(inp, "|ok|", state()));
        } catch (bytes memory reason) {
            vm.writeLine(f, string.concat(inp, "|err|", reasonOf(reason), "|", state()));
        }
    }
}
