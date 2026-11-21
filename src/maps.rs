//! Six symmetric 8x8 boards, each balanced (180° rotational or left-right mirror
//! symmetry with colours swapped) and designed for distinct strategic archetypes.

use crate::game::Board;
use rand::seq::IndexedRandom;

pub struct Map {
    pub name: &'static str,
    pub rows: &'static [&'static str],
}

pub const MAPS: &[Map] = &[
    Map {
        name: "Rift",
        rows: &[
            "R.......", //
            "........", //
            ".###....", //
            "........", //
            "........", //
            "....###.", //
            "........", //
            ".......G", //
        ],
    },
    Map {
        name: "Cascade",
        rows: &[
            "R.......", //
            ".....#..", //
            ".##..#..", //
            ".....#..", //
            "..#.....", //
            "..#..##.", //
            "..#.....", //
            ".......G", //
        ],
    },
    Map {
        name: "Pinwheel",
        rows: &[
            "R.......", //
            "..##....", //
            "......#.", //
            "......#.", //
            ".#......", //
            ".#......", //
            "....##..", //
            ".......G", //
        ],
    },
    Map {
        name: "Crossbar",
        rows: &[
            "R.......", //
            "...#....", //
            "...#....", //
            ".##.....", //
            ".....##.", //
            "....#...", //
            "....#...", //
            ".......G", //
        ],
    },
    Map {
        name: "Atoll",
        rows: &[
            "R.......", //
            "........", //
            "........", //
            ".#.##.#.", //
            ".#.##.#.", //
            "........", //
            "........", //
            ".......G", //
        ],
    },
    Map {
        name: "Compass",
        rows: &[
            "........", //
            "........", //
            "...##...", //
            "..#..#..", //
            "..#..#..", //
            "...##...", //
            "........", //
            "R......G", //
        ],
    },
];

pub fn random() -> (&'static str, Board) {
    let map = MAPS.choose(&mut rand::rng()).expect("at least one map");
    (map.name, Board::parse(map.rows))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::{Cell, Player};

    #[test]
    fn maps_are_fair() {
        let swap = |c: Cell| match c {
            Cell::Piece(p) => Cell::Piece(p.other()),
            c => c,
        };
        for map in MAPS {
            let b = Board::parse(map.rows);
            let (w, n) = (b.w, b.cells.len());
            // Half-turn or left-right mirror, with colours swapped.
            let half_turn = (0..n).all(|i| b.cells[n - 1 - i] == swap(b.cells[i]));
            let mirror = (0..n).all(|i| b.cells[(i / w) * w + (w - 1 - i % w)] == swap(b.cells[i]));
            assert!(half_turn || mirror, "{} is not symmetric", map.name);
            assert_eq!((b.w, b.h), (8, 8), "{}", map.name);
            assert_eq!(b.count(Player::Green), 1, "{}", map.name);
            assert_eq!(b.count(Player::Red), 1, "{}", map.name);
            assert!(b.has_moves(Player::Green) && b.has_moves(Player::Red));
        }
    }
}
