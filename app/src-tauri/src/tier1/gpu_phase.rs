//! Which GPU model is resident (PLAN "VRAM budget", decision 3). The worker runs embedding and
//! parsing in phases: outside the Fast profile the index server and the parser are never loaded
//! together, so the VRAM peak is about 2 GB instead of about 3.1 GB.
//!
//! Pure bookkeeping, no processes: the caller unloads what [`GpuPhase::begin`] names, then loads
//! its own model and later reports [`GpuPhase::end`].

use winseek_llama::Profile;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gpu {
    /// EmbeddingGemma-2 + mmproj (`Role::EmbedIndex`).
    Index,
    /// Infinity-Parser2 (`Role::Parser`).
    Parser,
}

#[derive(Debug)]
pub struct GpuPhase {
    profile: Profile,
    index: bool,
    parser: bool,
}

impl GpuPhase {
    pub fn new(profile: Profile) -> Self {
        GpuPhase { profile, index: false, parser: false }
    }

    fn loaded(&self, g: Gpu) -> bool {
        match g {
            Gpu::Index => self.index,
            Gpu::Parser => self.parser,
        }
    }

    fn set(&mut self, g: Gpu, on: bool) {
        match g {
            Gpu::Index => self.index = on,
            Gpu::Parser => self.parser = on,
        }
    }

    /// About to load `want`. Returns the other model that must be unloaded (and `end`ed) first, or
    /// `None` when it can be loaded right away. Marks `want` as loaded only once nothing blocks it;
    /// when a model is returned the caller unloads it, calls `end`, then calls `begin` again.
    pub fn begin(&mut self, want: Gpu) -> Option<Gpu> {
        let other = match want {
            Gpu::Index => Gpu::Parser,
            Gpu::Parser => Gpu::Index,
        };
        if self.profile != Profile::Fast && self.loaded(other) {
            return Some(other);
        }
        self.set(want, true);
        None
    }

    /// `g` has been unloaded.
    pub fn end(&mut self, g: Gpu) {
        self.set(g, false);
    }

    pub fn is_loaded(&self, g: Gpu) -> bool {
        self.loaded(g)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balanced_phases_index_and_parser() {
        for p in [Profile::Balanced, Profile::Low] {
            let mut g = GpuPhase::new(p);
            assert_eq!(g.begin(Gpu::Index), None);
            // The parser must wait for the index server to unload.
            assert_eq!(g.begin(Gpu::Parser), Some(Gpu::Index));
            assert!(!g.is_loaded(Gpu::Parser));
            g.end(Gpu::Index);
            assert_eq!(g.begin(Gpu::Parser), None);
            // ...and the other way round.
            assert_eq!(g.begin(Gpu::Index), Some(Gpu::Parser));
            g.end(Gpu::Parser);
            assert_eq!(g.begin(Gpu::Index), None);
            assert!(g.is_loaded(Gpu::Index) && !g.is_loaded(Gpu::Parser));
        }
    }

    #[test]
    fn fast_keeps_both_resident() {
        let mut g = GpuPhase::new(Profile::Fast);
        assert_eq!(g.begin(Gpu::Index), None);
        assert_eq!(g.begin(Gpu::Parser), None);
        assert!(g.is_loaded(Gpu::Index) && g.is_loaded(Gpu::Parser));
    }

    #[test]
    fn reloading_the_same_model_is_free() {
        let mut g = GpuPhase::new(Profile::Balanced);
        assert_eq!(g.begin(Gpu::Parser), None);
        assert_eq!(g.begin(Gpu::Parser), None);
    }
}
