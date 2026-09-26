/// Decides which frames get GPU timestamp queries.
///
/// The first frames are always measured (they include the cold run); after
/// that only every `STEADY_STRIDE`-th frame is, so steady-state frames skip the
/// query and resolve work.
#[derive(Debug, Default)]
pub(crate) struct ProfileSampler {
    frames: u64,
}

const COLD_FRAMES: u64 = 16;
const STEADY_STRIDE: u64 = 16;

impl ProfileSampler {
    pub(crate) fn reset(&mut self) {
        self.frames = 0;
    }

    /// Counts a frame and returns whether it should be measured.
    pub(crate) fn should_measure(&mut self) -> bool {
        let frame = self.frames;
        self.frames = self.frames.saturating_add(1);
        frame < COLD_FRAMES || (frame - COLD_FRAMES).is_multiple_of(STEADY_STRIDE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_cold_frames_then_every_stride() {
        let mut sampler = ProfileSampler::default();
        let measured: Vec<u64> = (0..50)
            .filter(|_| sampler.should_measure())
            .collect::<Vec<_>>();
        assert_eq!(measured.len(), 16 + 3);
        sampler.reset();
        assert!(sampler.should_measure());
    }
}
