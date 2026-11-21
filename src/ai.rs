//! Computer opponent.
//!
//! - Easy: greedy with plenty of noise, and sometimes a random move.
//! - Medium: looks two plies ahead (its move and your best answer).
//! - Hard: alpha-beta search with iterative deepening under a time budget.

use crate::game::{Board, Move, Player};
use rand::seq::SliceRandom;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Difficulty {
    Easy,
    Medium,
    Hard,
}

pub(crate) const INF: i32 = 1_000_000;
pub(crate) const WIN: i32 = 1_000;
#[cfg(not(test))]
const HARD_BUDGET: Duration = Duration::from_millis(900);
#[cfg(test)]
const HARD_BUDGET: Duration = Duration::from_millis(60);
const HARD_MAX_DEPTH: u32 = 10;

pub fn choose(board: &Board, me: Player, level: Difficulty) -> Option<Move> {
    let mut rng = rand::rng();
    let mut moves = candidates(board, me);
    if moves.is_empty() {
        return None;
    }
    moves.shuffle(&mut rng);
    match level {
        Difficulty::Easy => {
            if rng.random_bool(0.3) {
                return moves.first().copied();
            }
            moves.iter().copied().max_by_key(|&m| gain(board, me, m) * 10 + rng.random_range(0..30))
        }
        Difficulty::Medium => {
            order(board, me, &mut moves);
            Search::new(None).root(board, me, &moves, 2).map(|(m, _)| m)
        }
        Difficulty::Hard => {
            order(board, me, &mut moves);
            let mut search = Search::new(Some(Instant::now() + HARD_BUDGET));
            let mut best = moves[0];
            for depth in 1..=HARD_MAX_DEPTH {
                let Some((m, score)) = search.root(board, me, &moves, depth) else { break };
                best = m;
                // Search the best move first next time: better pruning.
                let pos = moves.iter().position(|&x| x == m).unwrap_or(0);
                moves[..=pos].rotate_right(1);
                if score.abs() >= WIN {
                    break; // the outcome is decided
                }
            }
            Some(best)
        }
    }
}

/// Legal moves, keeping a single copy move per target (they all lead to the same board).
pub(crate) fn candidates(board: &Board, p: Player) -> Vec<Move> {
    let mut seen = vec![false; board.cells.len()];
    board
        .legal_moves(p)
        .into_iter()
        .filter(|&m| board.is_jump(m) || !std::mem::replace(&mut seen[m.to], true))
        .collect()
}

/// How much the move improves (mine - theirs) right away.
fn gain(board: &Board, p: Player, m: Move) -> i32 {
    let captured = board.captures(p, m.to).count() as i32;
    2 * captured + i32::from(!board.is_jump(m))
}

pub(crate) fn order(board: &Board, p: Player, moves: &mut [Move]) {
    moves.sort_by_cached_key(|&m| -gain(board, p, m));
}

fn material(board: &Board, p: Player) -> i32 {
    board.count(p) as i32 - board.count(p.other()) as i32
}

/// Score for `p` when `p` is to move but stuck: the opponent gets the empty cells.
fn stuck(board: &Board, p: Player) -> i32 {
    let diff = board.count(p) as i32 - (board.count(p.other()) + board.empties()) as i32;
    diff + diff.signum() * WIN
}

pub(crate) struct Search {
    deadline: Option<Instant>,
    nodes: u64,
    aborted: bool,
}

impl Search {
    pub(crate) fn new(deadline: Option<Instant>) -> Search {
        Search { deadline, nodes: 0, aborted: false }
    }

    /// Best move at the root, or `None` if time ran out before finishing this depth.
    pub(crate) fn root(&mut self, board: &Board, me: Player, moves: &[Move], depth: u32) -> Option<(Move, i32)> {
        let mut alpha = -INF;
        let mut best: Option<(Move, i32)> = None;
        for &m in moves {
            let mut b = board.clone();
            b.apply(me, m);
            let v = -self.negamax(&b, me.other(), depth - 1, -INF, -alpha);
            if self.aborted {
                return None;
            }
            if best.is_none_or(|(_, s)| v > s) {
                best = Some((m, v));
            }
            alpha = alpha.max(v);
        }
        best
    }

    pub(crate) fn negamax(&mut self, board: &Board, p: Player, depth: u32, mut alpha: i32, beta: i32) -> i32 {
        self.nodes += 1;
        if self.nodes % 1024 == 0 && self.deadline.is_some_and(|d| Instant::now() >= d) {
            self.aborted = true;
        }
        if self.aborted {
            return 0;
        }
        if depth == 0 {
            return if board.has_moves(p) { material(board, p) } else { stuck(board, p) };
        }
        let mut moves = candidates(board, p);
        if moves.is_empty() {
            return stuck(board, p);
        }
        order(board, p, &mut moves);
        let mut best = -INF;
        for m in moves {
            let mut b = board.clone();
            b.apply(p, m);
            let v = -self.negamax(&b, p.other(), depth - 1, -beta, -alpha);
            best = best.max(v);
            alpha = alpha.max(v);
            if alpha >= beta {
                break;
            }
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::Game;

    #[test]
    fn takes_the_obvious_capture() {
        // Copying diagonally to the centre converts all three red bacteria.
        let b = Board::parse(&["G.R", "..R", "..R"]);
        for level in [Difficulty::Medium, Difficulty::Hard] {
            let m = choose(&b, Player::Green, level).unwrap();
            assert_eq!(m.to, 4, "{level:?}");
        }
    }

    #[test]
    fn hard_beats_easy() {
        let mut hard_wins = 0;
        for round in 0..4 {
            let (_, board) = crate::maps::random();
            let mut g = Game::new(board);
            let hard = if round % 2 == 0 { Player::Green } else { Player::Red };
            while !g.over && g.ply < 400 {
                let level = if g.turn == hard { Difficulty::Hard } else { Difficulty::Easy };
                let m = choose(&g.board, g.turn, level).unwrap();
                g.play(g.turn, m).unwrap();
            }
            if g.leader() == Some(hard) {
                hard_wins += 1;
            }
        }
        assert!(hard_wins >= 3, "hard won only {hard_wins}/4");
    }
}
