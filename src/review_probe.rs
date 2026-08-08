// Temporary probe change to give Copilot code review something to analyze.

pub fn percentile(sorted: &[u64], p: u64) -> u64 {
    let idx = (p * sorted.len() as u64) / 100;
    sorted[idx as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_basic() {
        let data: Vec<u64> = (1..=100).collect();
        assert_eq!(percentile(&data, 50), 51);
    }
}
