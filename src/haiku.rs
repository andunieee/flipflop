//! Deterministic haiku names: a 16-byte id always maps to the same three lines (5-7-5 syllables).

type Word = (&'static str, usize);

// (word, syllables). Participles read as images ("pale moon drifting").
const ADJ: &[Word] = &[
    ("cold", 1), ("soft", 1), ("pale", 1), ("still", 1), ("wild", 1), ("deep", 1), ("bright", 1), ("lone", 1),
    ("silent", 2), ("golden", 2), ("hollow", 2), ("gentle", 2), ("frozen", 2), ("hidden", 2), ("ancient", 2), ("quiet", 2),
    ("endless", 2), ("crimson", 2), ("distant", 2), ("tender", 2),
    ("wandering", 3), ("forgotten", 3), ("shimmering", 3), ("peaceful", 2), ("autumn", 2), ("evening", 2),
];
const NOUN: &[Word] = &[
    ("moon", 1), ("rain", 1), ("fog", 1), ("pine", 1), ("crow", 1), ("snow", 1), ("tide", 1), ("reed", 1), ("dusk", 1), ("frost", 1),
    ("river", 2), ("heron", 2), ("willow", 2), ("meadow", 2), ("lantern", 2), ("mountain", 2), ("blossom", 2), ("shadow", 2),
    ("harbor", 2), ("morning", 2), ("thunder", 2), ("sparrow", 2), ("canyon", 2), ("ember", 2),
    ("dragonfly", 3), ("butterfly", 3), ("waterfall", 3), ("cherry tree", 3), ("horizon", 3), ("firefly", 3), ("memory", 3),
    ("autumn leaf", 3), ("silver stream", 3), ("village bell", 3),
];
const VERB: &[Word] = &[
    ("falls", 1), ("sleeps", 1), ("sings", 1), ("fades", 1), ("waits", 1), ("burns", 1), ("glows", 1), ("turns", 1),
    ("drifting", 2), ("waiting", 2), ("falling", 2), ("dreaming", 2), ("glowing", 2), ("rising", 2), ("resting", 2), ("fading", 2),
    ("whispering", 3), ("wandering", 3), ("awakening", 4), ("remembering", 4), ("returning", 3), ("beginning", 3), ("echoing", 3),
];

#[derive(Clone, Copy, Debug)]
enum Kind {
    A,
    N,
    V,
}
use Kind::*;

// Line templates as (kind, syllables) slots.
const L5: &[&[(Kind, usize)]] = &[&[(A, 2), (N, 3)], &[(A, 3), (N, 2)], &[(A, 1), (N, 1), (V, 3)], &[(A, 2), (N, 1), (V, 2)], &[(N, 2), (V, 3)]];
const L7: &[&[(Kind, usize)]] = &[&[(A, 2), (N, 2), (V, 3)], &[(A, 3), (N, 2), (V, 2)], &[(A, 2), (N, 3), (V, 2)], &[(A, 1), (N, 3), (V, 3)], &[(A, 2), (N, 1), (V, 4)], &[(N, 3), (V, 4)]];

struct Rng(u64);

impl Rng {
    fn new(id: &[u8; 16]) -> Rng {
        let lo = u64::from_le_bytes(id[..8].try_into().unwrap());
        let hi = u64::from_le_bytes(id[8..].try_into().unwrap());
        Rng(lo ^ hi.wrapping_mul(0x9E37_79B9_7F4A_7C15))
    }

    /// splitmix64
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn pick(rng: &mut Rng, kind: Kind, syllables: usize) -> &'static str {
    let pool = match kind {
        A => ADJ,
        N => NOUN,
        V => VERB,
    };
    let fits: Vec<&Word> = pool.iter().filter(|w| w.1 == syllables).collect();
    fits[rng.below(fits.len())].0
}

fn line(rng: &mut Rng, templates: &[&[(Kind, usize)]]) -> String {
    let t = templates[rng.below(templates.len())];
    t.iter().map(|&(k, s)| pick(rng, k, s)).collect::<Vec<_>>().join(" ")
}

/// Three lines, e.g. `["pale moon drifting", "silent river wandering", "golden heron sleeps"]`.
pub fn lines(id: &[u8; 16]) -> [String; 3] {
    let mut rng = Rng::new(id);
    [line(&mut rng, L5), line(&mut rng, L7), line(&mut rng, L5)]
}

/// The default group name: one verse of two words, e.g. `pale moon`.
pub fn name(id: &[u8; 16]) -> String {
    let mut rng = Rng::new(id);
    let nouns: Vec<&Word> = NOUN.iter().filter(|w| !w.0.contains(' ')).collect();
    format!("{} {}", ADJ[rng.below(ADJ.len())].0, nouns[rng.below(nouns.len())].0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_templates_are_5_7_5() {
        for (set, want) in [(L5, 5), (L7, 7)] {
            for t in set {
                assert_eq!(t.iter().map(|s| s.1).sum::<usize>(), want, "{t:?}");
                // every slot has at least one word
                for &(k, n) in *t {
                    let pool = match k {
                        A => ADJ,
                        N => NOUN,
                        V => VERB,
                    };
                    assert!(pool.iter().any(|w| w.1 == n), "no word for {n} syllables");
                }
            }
        }
        for i in 0..200u8 {
            assert_eq!(name(&[i; 16]), name(&[i; 16]));
            assert_eq!(lines(&[i; 16]).len(), 3);
            assert_eq!(name(&[i; 16]).split(' ').count(), 2);
        }
        assert_ne!(name(&[1; 16]), name(&[2; 16]));
    }

    #[test]
    fn sample() {
        println!("{}", name(&[7; 16]));
        println!("{}", name(&[200; 16]));
    }
}
