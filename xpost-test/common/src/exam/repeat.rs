//! Five independent passes of one rate grid.
//!
//! A pass stores its seed and which arm boots first. Both arms share that
//! pass's rate order. Odd passes boot HAL first. Even passes boot XDP first.

use crate::db::Firmware;

pub const ROUNDS: i32 = 5;
const MASTER: u64 = 0x2026_1019;

#[derive(Clone, Debug, PartialEq)]
pub struct RoundPlan {
    pub round: i32,
    pub seed: i64,
    pub hal_first: bool,
    pub rates: Vec<f64>,
}

impl RoundPlan {
    pub fn order_firmware(&self, arms: &[Firmware]) -> Vec<Firmware> {
        let mut ordered = arms.to_vec();
        ordered.sort_by_key(|fw| fw.is_hal() != self.hal_first);
        ordered
    }
}

pub fn plans(rates: &[f64]) -> Vec<RoundPlan> {
    (1..=ROUNDS)
        .map(|round| {
            let mut state = MASTER
                ^ ((round as u64) << 32)
                ^ (round as u64).wrapping_mul(0x9E37_79B9);
            let seed = (state & i64::MAX as u64) as i64;
            let mut order = rates.to_vec();
            for i in (1..order.len()).rev() {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1);
                let j = (state as usize) % (i + 1);
                order.swap(i, j);
            }
            RoundPlan {
                round,
                seed,
                hal_first: round % 2 == 1,
                rates: order,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn five_passes_cover_every_rate_once() {
        let rates = [400.0, 1400.0, 4000.0];
        let first = plans(&rates);
        let second = plans(&rates);
        assert_eq!(first, second);
        assert_eq!(first.len(), 5);
        let mut seeds = Vec::new();
        for plan in &first {
            assert_eq!(plan.rates.len(), rates.len());
            let mut got = plan.rates.clone();
            got.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let mut expect = rates.to_vec();
            expect.sort_by(|a, b| a.partial_cmp(b).unwrap());
            assert_eq!(got, expect, "round {} is not a permutation", plan.round);
            seeds.push(plan.seed);
        }
        seeds.sort();
        seeds.dedup();
        assert_eq!(seeds.len(), 5);
        assert!(first[0].hal_first);
        assert!(!first[1].hal_first);
    }

    #[test]
    fn arm_order_puts_the_recorded_arm_first() {
        let arms = [Firmware::Hal, Firmware::XdpCag];
        let plan = &plans(&[400.0])[0];
        assert!(plan.hal_first);
        assert_eq!(plan.order_firmware(&arms)[0], Firmware::Hal);
        let xdp_first = &plans(&[400.0])[1];
        assert_eq!(xdp_first.order_firmware(&arms)[0], Firmware::XdpCag);
    }
}
