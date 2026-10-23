//! Differential test: `pool::oracle` must match `diff-tests/src/libraries/Oracle.sol` exactly.
//!
//! The vectors are a stateful replay log written by the Foundry generator
//! (`diff-tests/test/GenerateOracle.t.sol`):
//! `cd diff-tests && forge test --match-test testGenerateOracle`
//!
//! Line kinds:
//! - `reset`: a fresh harness; Rust clears its state.
//! - `<op>|args|ok|result` or `<op>|args|err|REVERT_REASON`: one call. A reason of `I` or `OLD`
//!   maps to `OracleNotInitialized` / `ObservationTooOld`; an empty reason is an EVM
//!   division by zero (`DivisionByZero`).
//! - `st|index|cardinality|cardinalityNext|ts|tick|spl|initialized`: state after the call.
//! - `dump|k:ts:tick:spl:init|...`: every slot below `cardinalityNext`.

use std::collections::BTreeMap;
use std::fmt::Display;
use std::fs;
use std::path::PathBuf;

use windswap_engine::pool::oracle::{self, Observation, Observations};
use windswap_engine::{MathError, U256};

#[derive(Default)]
struct Replay {
    obs: Observations,
    index: u16,
    cardinality: u16,
    cardinality_next: u16,
    last_time: Option<u32>,
    counts: BTreeMap<String, usize>,
}

fn p16(s: &str) -> u16 {
    s.parse().unwrap()
}
fn p32(s: &str) -> u32 {
    s.parse().unwrap()
}
fn pi32(s: &str) -> i32 {
    s.parse().unwrap()
}
fn pi64(s: &str) -> i64 {
    s.parse().unwrap()
}
fn pu128(s: &str) -> u128 {
    s.parse().unwrap()
}
fn pu256(s: &str) -> U256 {
    s.parse().unwrap()
}

fn join<T: Display>(values: &[T]) -> String {
    values
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// Maps a Solidity revert reason to the expected Rust error.
fn expect_err(at: &str, reason: &str, got: MathError) {
    let want = match reason {
        "I" => MathError::OracleNotInitialized,
        "OLD" => MathError::ObservationTooOld,
        "" => MathError::DivisionByZero,
        other => panic!("{at}: unknown revert reason {other:?}"),
    };
    assert_eq!(got, want, "{at}");
}

impl Replay {
    fn bump(&mut self, key: impl Into<String>) {
        *self.counts.entry(key.into()).or_default() += 1;
    }

    fn run(&mut self, no: usize, f: &[&str]) {
        let at = format!("oracle.txt:{}: {}", no + 1, f.join("|"));
        match f[0] {
            "reset" => {
                self.obs.clear();
                self.index = 0;
                self.cardinality = 0;
                self.cardinality_next = 0;
                self.last_time = None;
            }
            "dump" => self.check_dump(&at, &f[1..]),
            "st" => self.check_state(&at, &f[1..]),
            op => self.op(&at, op, f),
        }
    }

    fn op(&mut self, at: &str, op: &str, f: &[&str]) {
        let pos = f
            .iter()
            .position(|s| *s == "ok" || *s == "err")
            .unwrap_or_else(|| panic!("{at}: no status field"));
        let args = &f[1..pos];
        let status = f[pos];
        let expected = f.get(pos + 1).copied().unwrap_or("");

        let result = match op {
            "init" => {
                let time = p32(args[0]);
                let (c, n) = oracle::initialize(&mut self.obs, time);
                self.index = 0;
                self.cardinality = c;
                self.cardinality_next = n;
                self.last_time = Some(time);
                Ok(format!("{c},{n}"))
            }
            "write" => self.write(args),
            "grow" => {
                let next = p16(args[0]);
                let current = self.cardinality_next;
                let res = oracle::grow(&mut self.obs, current, next);
                if let Ok(n) = res {
                    if next <= current {
                        self.bump("grow.noop");
                    }
                    self.cardinality_next = n;
                }
                res.map(|n| n.to_string())
            }
            "obsone" => {
                let (time, ago) = (p32(args[0]), p32(args[1]));
                if ago > time {
                    self.bump("obsone.target_wrapped");
                }
                oracle::observe_single(
                    &self.obs,
                    time,
                    ago,
                    pi32(args[2]),
                    self.index,
                    pu128(args[3]),
                    self.cardinality,
                )
                .map(|(t, s)| format!("{t},{s}"))
            }
            "obs" => {
                let agos: Vec<u32> = args[3].split(',').map(p32).collect();
                oracle::observe(
                    &self.obs,
                    p32(args[0]),
                    &agos,
                    pi32(args[1]),
                    self.index,
                    pu128(args[2]),
                    self.cardinality,
                )
                .map(|(t, s)| format!("{};{}", join(&t), join(&s)))
            }
            other => panic!("{at}: unknown op {other:?}"),
        };
        self.compare(at, op, status, expected, result);
    }

    fn write(&mut self, args: &[&str]) -> Result<String, MathError> {
        let time = p32(args[0]);
        let tick = pi32(args[1]);
        let liquidity = pu128(args[2]);
        let prev = self.obs.get(&self.index).copied().unwrap_or_default();
        let same_block = prev.block_timestamp == time;
        let res = oracle::write(
            &mut self.obs,
            self.index,
            time,
            tick,
            liquidity,
            self.cardinality,
            self.cardinality_next,
        );
        if let Ok((index, cardinality)) = res {
            if same_block {
                self.bump("write.same_block");
            } else {
                // untruncated accumulators: a value outside the Solidity width means it wrapped
                let delta = time.wrapping_sub(prev.block_timestamp);
                let raw_tick =
                    i128::from(prev.tick_cumulative) + i128::from(tick) * i128::from(delta);
                if !(-(1_i128 << 55)..(1_i128 << 55)).contains(&raw_tick) {
                    self.bump("write.tick56_wrapped");
                }
                let raw_spl = prev.seconds_per_liquidity_cumulative_x128
                    + (U256::from(delta) << 128_usize) / U256::from(liquidity.max(1));
                if raw_spl >= U256::from(1_u8) << 160_usize {
                    self.bump("write.spl160_wrapped");
                }
            }
            if self.last_time.is_some_and(|t| time < t) {
                self.bump("write.time_wrapped");
            }
            if index < self.index {
                self.bump("write.index_wrapped");
            }
            if cardinality != self.cardinality {
                self.bump("write.cardinality_bumped");
            }
            self.index = index;
            self.cardinality = cardinality;
            self.last_time = Some(time);
        }
        res.map(|(index, cardinality)| format!("{index},{cardinality}"))
    }

    fn compare(
        &mut self,
        at: &str,
        op: &str,
        status: &str,
        expected: &str,
        got: Result<String, MathError>,
    ) {
        match (status, got) {
            ("ok", Ok(got)) => {
                assert_eq!(got, expected, "{at}");
                self.bump(format!("{op}.ok"));
            }
            ("err", Err(e)) => {
                expect_err(at, expected, e);
                let reason = if expected.is_empty() {
                    "DIV0"
                } else {
                    expected
                };
                self.bump(format!("{op}.err.{reason}"));
            }
            ("ok", Err(e)) => panic!("{at}: Solidity returned {expected}, Rust errored {e:?}"),
            (_, Ok(got)) => {
                panic!("{at}: Solidity reverted with {expected:?}, Rust returned {got}")
            }
            (other, _) => panic!("{at}: bad status {other:?}"),
        }
    }

    fn check_state(&self, at: &str, f: &[&str]) {
        assert_eq!(self.index, p16(f[0]), "{at}: index");
        assert_eq!(self.cardinality, p16(f[1]), "{at}: cardinality");
        assert_eq!(self.cardinality_next, p16(f[2]), "{at}: cardinalityNext");
        let expected = Observation {
            block_timestamp: p32(f[3]),
            tick_cumulative: pi64(f[4]),
            seconds_per_liquidity_cumulative_x128: pu256(f[5]),
            initialized: f[6] == "1",
        };
        let actual = self.obs.get(&self.index).copied().unwrap_or_default();
        assert_eq!(actual, expected, "{at}: observation at index");
    }

    fn check_dump(&self, at: &str, fields: &[&str]) {
        assert_eq!(
            fields.len(),
            usize::from(self.cardinality_next),
            "{at}: slot count"
        );
        for (k, field) in fields.iter().enumerate() {
            let p: Vec<&str> = field.split(':').collect();
            assert_eq!(p[0], k.to_string(), "{at}: slot index");
            let expected = Observation {
                block_timestamp: p32(p[1]),
                tick_cumulative: pi64(p[2]),
                seconds_per_liquidity_cumulative_x128: pu256(p[3]),
                initialized: p[4] == "1",
            };
            let slot = u16::try_from(k).unwrap();
            let actual = self.obs.get(&slot).copied().unwrap_or_default();
            assert_eq!(actual, expected, "{at}: slot {k}");
        }
        assert!(
            self.obs.keys().all(|&k| k < self.cardinality_next),
            "{at}: Rust holds a slot at or beyond cardinalityNext"
        );
    }

    fn report(&self) {
        for (key, n) in &self.counts {
            eprintln!("{key:>32}: {n}");
        }
        for key in [
            "init.ok",
            "write.ok",
            "write.same_block",
            "write.time_wrapped",
            "write.index_wrapped",
            "write.cardinality_bumped",
            "write.tick56_wrapped",
            "write.spl160_wrapped",
            "grow.ok",
            "grow.noop",
            "grow.err.I",
            "obsone.ok",
            "obsone.err.OLD",
            "obsone.target_wrapped",
            "obs.ok",
            "obs.err.OLD",
            "obs.err.I",
        ] {
            assert!(
                self.counts.get(key).copied().unwrap_or(0) > 0,
                "vector set never exercises {key}"
            );
        }
    }
}

#[test]
fn oracle_matches_solidity_replay() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../diff-tests/vectors/oracle.txt");
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut replay = Replay::default();
    for (no, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split('|').collect();
        replay.run(no, &fields);
    }
    replay.report();
}
