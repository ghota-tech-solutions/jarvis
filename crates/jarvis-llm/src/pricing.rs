//! Per-model pricing lookup.
//!
//! Costs are sourced from the `ModelRegistry` (which loads them from
//! `jarvis.toml`'s `cost_per_mtok_in` / `cost_per_mtok_out` fields). Local
//! models default to 0/0 unless the user sets a synthetic cost.
//!
//! `estimate_usd` is the canonical token-count → dollar conversion used by
//! the cost-breakdown RPC and the cost HUD in the SPA.

use crate::ModelRegistry;
use jarvis_core::ProviderName;

/// Convert raw token counts at a model's per-million-token rates into USD.
/// Returns 0.0 if the model is unknown or the rates are zero.
pub fn estimate_usd(
    registry: &ModelRegistry,
    model: &ProviderName,
    tokens_in: u64,
    tokens_out: u64,
) -> f64 {
    let entry = match registry.get(model) {
        Some(e) => e,
        None => return 0.0,
    };
    let m_in = tokens_in as f64 / 1_000_000.0;
    let m_out = tokens_out as f64 / 1_000_000.0;
    m_in * entry.cost_per_mtok_in + m_out * entry.cost_per_mtok_out
}

/// (cost_in_per_mtok, cost_out_per_mtok) for a model, or (0, 0) if unknown.
pub fn rates(registry: &ModelRegistry, model: &ProviderName) -> (f64, f64) {
    registry
        .get(model)
        .map(|e| (e.cost_per_mtok_in, e.cost_per_mtok_out))
        .unwrap_or((0.0, 0.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ModelKind, ModelRegistry, make_openai_compat_entry};
    use jarvis_core::Capabilities;

    #[test]
    fn estimate_matches_per_million_rates() {
        let mut r = ModelRegistry::new();
        r.insert(make_openai_compat_entry(
            "test".to_string(),
            ModelKind::Remote,
            "http://x".to_string(),
            "test".to_string(),
            String::new(),
            5,
            Capabilities::default(),
            0.27,
            1.10,
        ));
        // make_openai_compat_entry prepends "<kind>:" so the qualified name is
        // "remote:test".
        let name = ProviderName::new("remote:test".to_string());
        // 1M in + 1M out → 0.27 + 1.10 = 1.37
        let usd = estimate_usd(&r, &name, 1_000_000, 1_000_000);
        assert!((usd - 1.37).abs() < 1e-9);
        // 500k in + 250k out → 0.135 + 0.275 = 0.41
        let usd = estimate_usd(&r, &name, 500_000, 250_000);
        assert!((usd - 0.41).abs() < 1e-9);
    }

    #[test]
    fn unknown_model_costs_zero() {
        let r = ModelRegistry::new();
        let name = ProviderName::new("unknown".to_string());
        assert_eq!(estimate_usd(&r, &name, 999_999, 999_999), 0.0);
    }
}
