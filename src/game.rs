//! Rules of the game. Pure logic, no I/O.
//!
//! Same rules as noxio (github.com/thalkz/noxio, lib/rules.dart):
//! - A move takes one of your bacteria to an empty cell. To any of the 8 neighbouring
//!   cells (diagonals included) it copies itself. Exactly two cells away in a straight
//!   line (no diagonals) it jumps, over anything, and its old cell empties.
//! - The bacterium that lands converts every enemy bacterium around it (8 neighbours).
//! - When the player to move has no legal move, the game ends and every empty
//!   cell goes to the other player.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Player {
    Green,
    Red,
}

impl Player {
    pub const ALL: [Player; 2] = [Player::Green, Player::Red];

    pub fn other(self) -> Player {
        match self {
            Player::Green => Player::Red,
            Player::Red => Player::Green,
        }
    }

    pub fn idx(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cell {
    Empty,
    Blocked,
    Piece(Player),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Move {
    pub from: usize,
    pub to: usize,
}

const AROUND: [(i32, i32); 8] = [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)];
/// Copies to the 8 neighbours, then the 4 straight jumps.
const STEPS: [(i32, i32); 12] = [
    (-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1),
    (2, 0), (-2, 0), (0, 2), (0, -2),
];

#[derive(Clone, Debug)]
pub struct Board {
    pub w: usize,
    pub h: usize,
    pub cells: Vec<Cell>,
}

impl Board {
    /// Rows use `.` empty, `#` blocked, `G` green, `R` red.
    pub fn parse(rows: &[&str]) -> Board {
        let h = rows.len();
        let w = rows[0].len();
        assert!(rows.iter().all(|r| r.len() == w), "ragged map");
        Board::decode(w, h, &rows.concat()).expect("bad map")
    }

    /// The reverse of `encode`. `None` if the cells don't make a `w` x `h` board.
    pub fn decode(w: usize, h: usize, cells: &str) -> Option<Board> {
        let cells: Vec<Cell> = cells
            .chars()
            .map(|c| match c {
                '.' => Some(Cell::Empty),
                '#' => Some(Cell::Blocked),
                'G' => Some(Cell::Piece(Player::Green)),
                'R' => Some(Cell::Piece(Player::Red)),
                _ => None,
            })
            .collect::<Option<_>>()?;
        (w > 0 && cells.len() == w * h).then_some(Board { w, h, cells })
    }

    pub fn encode(&self) -> String {
        self.cells
            .iter()
            .map(|c| match c {
                Cell::Empty => '.',
                Cell::Blocked => '#',
                Cell::Piece(Player::Green) => 'G',
                Cell::Piece(Player::Red) => 'R',
            })
            .collect()
    }

    fn offset(&self, i: usize, dx: i32, dy: i32) -> Option<usize> {
        let x = (i % self.w) as i32 + dx;
        let y = (i / self.w) as i32 + dy;
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            None
        } else {
            Some(y as usize * self.w + x as usize)
        }
    }

    pub fn count(&self, p: Player) -> usize {
        self.cells.iter().filter(|&&c| c == Cell::Piece(p)).count()
    }

    pub fn empties(&self) -> usize {
        self.cells.iter().filter(|&&c| c == Cell::Empty).count()
    }

    pub fn is_jump(&self, m: Move) -> bool {
        let dx = (m.from % self.w).abs_diff(m.to % self.w);
        let dy = (m.from / self.w).abs_diff(m.to / self.w);
        dx.max(dy) == 2
    }

    pub fn is_legal(&self, p: Player, m: Move) -> bool {
        let n = self.cells.len();
        if m.from >= n || m.to >= n {
            return false;
        }
        if self.cells[m.from] != Cell::Piece(p) || self.cells[m.to] != Cell::Empty {
            return false;
        }
        let dx = (m.from % self.w).abs_diff(m.to % self.w);
        let dy = (m.from / self.w).abs_diff(m.to / self.w);
        matches!((dx, dy), (1, 0) | (0, 1) | (1, 1) | (2, 0) | (0, 2))
    }

    pub fn legal_moves(&self, p: Player) -> Vec<Move> {
        let mut moves = Vec::new();
        for (from, &c) in self.cells.iter().enumerate() {
            if c != Cell::Piece(p) {
                continue;
            }
            for &(dx, dy) in &STEPS {
                if let Some(to) = self.offset(from, dx, dy) {
                    if self.cells[to] == Cell::Empty {
                        moves.push(Move { from, to });
                    }
                }
            }
        }
        moves
    }

    pub fn has_moves(&self, p: Player) -> bool {
        self.cells.iter().enumerate().any(|(from, &c)| {
            c == Cell::Piece(p)
                && STEPS.iter().any(|&(dx, dy)| {
                    self.offset(from, dx, dy).is_some_and(|to| self.cells[to] == Cell::Empty)
                })
        })
    }

    /// Enemy bacteria a piece landing on `to` would convert.
    pub fn captures(&self, p: Player, to: usize) -> impl Iterator<Item = usize> + '_ {
        let enemy = Cell::Piece(p.other());
        AROUND
            .iter()
            .filter_map(move |&(dx, dy)| self.offset(to, dx, dy))
            .filter(move |&i| self.cells[i] == enemy)
    }

    /// Applies a move assumed legal. Returns the converted cells.
    pub fn apply(&mut self, p: Player, m: Move) -> Vec<usize> {
        if self.is_jump(m) {
            self.cells[m.from] = Cell::Empty;
        }
        self.cells[m.to] = Cell::Piece(p);
        let converted: Vec<usize> = self.captures(p, m.to).collect();
        for &i in &converted {
            self.cells[i] = Cell::Piece(p);
        }
        converted
    }
}

#[derive(Clone, Debug)]
pub struct LastMove {
    pub by: Player,
    pub from: usize,
    pub to: usize,
    pub jump: bool,
    pub converted: Vec<usize>,
    /// Empty cells handed over when the game auto-completed.
    pub filled: Vec<usize>,
}

#[derive(Clone, Debug)]
pub struct Game {
    /// The position before the first move, kept for replays.
    pub start: Board,
    pub moves: Vec<Move>,
    pub board: Board,
    pub turn: Player,
    pub ply: u32,
    pub last: Option<LastMove>,
    /// The board is complete or a player is stuck: scores are final.
    pub over: bool,
}

impl Game {
    pub fn new(board: Board) -> Game {
        Game { start: board.clone(), moves: Vec::new(), board, turn: Player::Green, ply: 0, last: None, over: false }
    }

    pub fn play(&mut self, p: Player, m: Move) -> Result<(), &'static str> {
        if self.over {
            return Err("game is over");
        }
        if p != self.turn {
            return Err("not your turn");
        }
        if !self.board.is_legal(p, m) {
            return Err("illegal move");
        }
        let jump = self.board.is_jump(m);
        let converted = self.board.apply(p, m);
        self.ply += 1;
        self.moves.push(m);

        let mut filled = Vec::new();
        if self.board.has_moves(p.other()) {
            self.turn = p.other();
        } else {
            // The opponent is stuck: the remaining space is ours.
            for (i, c) in self.board.cells.iter_mut().enumerate() {
                if *c == Cell::Empty {
                    *c = Cell::Piece(p);
                    filled.push(i);
                }
            }
            self.over = true;
        }
        self.last = Some(LastMove { by: p, from: m.from, to: m.to, jump, converted, filled });
        Ok(())
    }

    pub fn score(&self, p: Player) -> usize {
        self.board.count(p)
    }

    pub fn leader(&self) -> Option<Player> {
        let (g, r) = (self.score(Player::Green), self.score(Player::Red));
        match g.cmp(&r) {
            std::cmp::Ordering::Greater => Some(Player::Green),
            std::cmp::Ordering::Less => Some(Player::Red),
            std::cmp::Ordering::Equal => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clone_and_jump() {
        let mut g = Game::new(Board::parse(&["G....", ".....", "....R"]));
        assert!(g.play(Player::Green, Move { from: 0, to: 1 }).is_ok());
        assert_eq!(g.board.encode(), format!("GG{}R", ".".repeat(12)));
        assert_eq!(g.turn, Player::Red);
        // Red jumps two cells up: its old cell empties.
        assert!(g.play(Player::Red, Move { from: 14, to: 4 }).is_ok());
        assert_eq!(g.board.cells[14], Cell::Empty);
        assert_eq!(g.board.cells[4], Cell::Piece(Player::Red));
    }

    #[test]
    fn move_shapes() {
        let b = Board::parse(&["G....", ".....", "....R"]);
        assert!(b.is_legal(Player::Green, Move { from: 0, to: 6 })); // diagonal copy
        assert!(!b.is_jump(Move { from: 0, to: 6 }));
        assert!(b.is_legal(Player::Green, Move { from: 0, to: 10 })); // straight jump
        assert!(b.is_jump(Move { from: 0, to: 10 }));
        assert!(!b.is_legal(Player::Green, Move { from: 0, to: 12 })); // diagonal jump
        assert!(!b.is_legal(Player::Green, Move { from: 0, to: 7 })); // knight move
        assert!(!b.is_legal(Player::Green, Move { from: 0, to: 3 })); // 3 cells
        assert!(!b.is_legal(Player::Red, Move { from: 0, to: 1 })); // not your piece
    }

    #[test]
    fn converts_diagonals() {
        let mut g = Game::new(Board::parse(&["G.R", "...", "R.R"]));
        g.play(Player::Green, Move { from: 0, to: 3 }).unwrap(); // lands at (0,1)
        let last = g.last.as_ref().unwrap();
        assert_eq!(last.converted, vec![6]); // the red piece right below
        assert_eq!(g.board.cells[6], Cell::Piece(Player::Green));
        g.play(Player::Red, Move { from: 2, to: 1 }).unwrap(); // lands at (1,0)
        let last = g.last.as_ref().unwrap();
        // (0,0) is beside it, (0,1) is diagonal: both convert.
        assert!(last.converted.contains(&0) && last.converted.contains(&3));
    }

    #[test]
    fn stuck_player_hands_over_empty_cells() {
        // Red is walled in, diagonals included, and its only jump lands on a wall.
        let mut g = Game::new(Board::parse(&["G.##R", "..###"]));
        g.play(Player::Green, Move { from: 0, to: 1 }).unwrap();
        assert!(g.over);
        assert_eq!(g.score(Player::Green), 4);
        assert_eq!(g.score(Player::Red), 1);
        assert_eq!(g.last.unwrap().filled, vec![5, 6]);
    }

    #[test]
    fn board_full_ends_game() {
        let mut g = Game::new(Board::parse(&["G.R"]));
        g.play(Player::Green, Move { from: 0, to: 1 }).unwrap();
        assert!(g.over);
        assert_eq!(g.leader(), Some(Player::Green));
    }
}
