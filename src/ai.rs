//! Computer opponent and search engine.
//!
//! - Easy: greedy with plenty of noise, and sometimes a random move.
//! - Medium: 2-ply search with lighter strategic evaluation.
//! - Hard: bitboard PVS alpha-beta with transposition table, killer moves,
//!   iterative deepening, and strategic evaluation (territory, mobility, safety).
//!
//! All difficulty levels detect hopeless or trapped positions and resign rather
//! than jumping back and forth pointlessly.

use crate::game::{Board, Cell, Move, Player};
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

/// Evaluation scale: 100 centi-cells = 1.00 cell advantage (like chess centipawns).
pub const SCALE: i32 = 100;
pub(crate) const INF: i32 = 1_000_000;
/// Base score for a forced win (adjusted by margin and ply distance).
pub(crate) const WIN_SCORE: i32 = 50_000;
/// Any score with `|score| >= WIN` represents a forced win or loss.
pub const WIN: i32 = 40_000;
/// Logistic steepness mapping centi-cells to win probability (matches ~Stockfish/Lichess scale).
pub const WIN_PROB_K: f64 = 0.0038;

#[cfg(not(test))]
const HARD_BUDGET: Duration = Duration::from_millis(850);
#[cfg(test)]
const HARD_BUDGET: Duration = Duration::from_millis(60);
const HARD_MAX_DEPTH: u32 = 18;
const MAX_PLY: usize = 32;
const TT_BITS: usize = 16;
const TT_SIZE: usize = 1 << TT_BITS;
const TT_MASK: usize = TT_SIZE - 1;

/// What the computer decides to do on its turn.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Decision {
    Move(Move),
    Resign,
}

/// Result of the computer's turn, including the updated hopeless-turn streak.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TurnOutcome {
    pub decision: Decision,
    pub hopeless_streak: u8,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum EvalMode {
    Full,
    Light,
}

/// Converts a centi-cell engine score (from a player's perspective) to win probability in `[0.0, 1.0]`.
pub fn win_prob(score: i32) -> f64 {
    if score >= WIN {
        1.0
    } else if score <= -WIN {
        0.0
    } else {
        1.0 / (1.0 + (-WIN_PROB_K * score as f64).exp())
    }
}

/// Deterministic 64-bit hash of a board position (independent of side to move).
pub fn board_hash(board: &Board) -> u64 {
    let mut h = 0x9e37_79b9_7f4a_7c15u64 ^ ((board.w as u64) << 8) ^ (board.h as u64);
    for (i, &c) in board.cells.iter().enumerate() {
        let code = match c {
            Cell::Empty => continue,
            Cell::Blocked => 3u64,
            Cell::Piece(Player::Green) => 1u64,
            Cell::Piece(Player::Red) => 2u64,
        };
        h ^= splitmix64((i as u64).wrapping_mul(4).wrapping_add(code));
    }
    h
}

/// Hash of only `p`'s piece positions, used to detect a player jumping back and forth
/// even while the opponent is filling cells elsewhere on the board.
pub fn side_hash(board: &Board, p: Player) -> u64 {
    let mut h = 0x243f_6a88_85a3_08d3u64;
    for (i, &c) in board.cells.iter().enumerate() {
        if c == Cell::Piece(p) {
            h ^= splitmix64((i as u64).wrapping_mul(4).wrapping_add(1));
        }
    }
    h
}

/// Recent position history passed to the AI to avoid loops and detect pointless shuffling.
#[derive(Clone, Default)]
pub struct History {
    pub boards: Vec<u64>,
    pub my_sides: Vec<u64>,
}

const fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// Chooses a move without resignation (used in tests).
#[cfg(test)]
pub fn choose(board: &Board, me: Player, level: Difficulty) -> Option<Move> {
    let moves = candidates(board, me);
    if moves.is_empty() {
        return None;
    }
    let (mv, _) = pick_move(board, me, level, &History::default())?;
    Some(mv)
}

/// Chooses whether to play a move or resign, considering position history and hopeless turns.
pub fn decide(
    board: &Board,
    me: Player,
    level: Difficulty,
    ply: u32,
    history: &History,
    hopeless_streak: u8,
) -> Option<TurnOutcome> {
    if !board.has_moves(me) {
        return None;
    }
    let empty_cnt = board.empties();

    // Never resign on the very last square — just finish the game cleanly on the board.
    if empty_cnt > 1 {
        if let Some(g) = Geom::from_board(board) {
            let (my, op) = g.bitboards(board, me);
            if is_mathematically_sealed_loss(&g, my, op) {
                return Some(TurnOutcome { decision: Decision::Resign, hopeless_streak: hopeless_streak.saturating_add(1) });
            }
        }
    }

    let (best_move, search_score) = pick_move(board, me, level, history)?;

    // Evaluate the real position strength (Hard already computed a deep score; Easy/Medium
    // do a tiny 4-ply bitboard check so they also recognize forced losses and hopelessness).
    let eval_score = match level {
        Difficulty::Hard => search_score,
        Difficulty::Easy | Difficulty::Medium => quick_eval(board, me),
    };

    if empty_cnt > 1 {
        let mut after = board.clone();
        let converted = after.apply(me, best_move);
        let is_jump = board.is_jump(best_move);
        let repeats_board = history.boards.contains(&board_hash(&after));
        let repeats_own = is_jump && converted.is_empty() && history.my_sides.contains(&side_hash(&after, me));

        // 1. Proven forced loss in search.
        let my_cnt = board.count(me);
        let op_cnt = board.count(me.other());
        if eval_score <= -WIN && (empty_cnt > 2 || repeats_board || repeats_own || my_cnt * 2 < op_cnt) {
            return Some(TurnOutcome { decision: Decision::Resign, hopeless_streak: hopeless_streak.saturating_add(1) });
        }

        // 2. Reduced to jumping back/forth or repeating a previous board while clearly losing.
        if (repeats_board || repeats_own) && eval_score <= -700 {
            return Some(TurnOutcome { decision: Decision::Resign, hopeless_streak: hopeless_streak.saturating_add(1) });
        }

        // 3. Overwhelmingly losing with no realistic counterplay for several consecutive turns.
        let my_mob = candidates(board, me).len();
        let hopeless_now = ply >= 14
            && empty_cnt >= 3
            && (eval_score <= -1800
                || (eval_score <= -1300 && (op_cnt >= my_cnt + 8 || (my_cnt <= 3 && op_cnt >= 12)))
                || (eval_score <= -1000 && my_mob <= 2));

        let next_streak = if hopeless_now { hopeless_streak.saturating_add(1) } else { 0 };
        if next_streak >= 3 {
            return Some(TurnOutcome { decision: Decision::Resign, hopeless_streak: next_streak });
        }
        return Some(TurnOutcome { decision: Decision::Move(best_move), hopeless_streak: next_streak });
    }

    Some(TurnOutcome { decision: Decision::Move(best_move), hopeless_streak: 0 })
}

fn quick_eval(board: &Board, me: Player) -> i32 {
    let moves = candidates(board, me);
    if moves.is_empty() {
        return -WIN_SCORE;
    }
    let mut search = Search::new(None);
    search.root(board, me, &moves, 4).map_or(0, |(_, s)| s)
}

fn pick_move(board: &Board, me: Player, level: Difficulty, history: &History) -> Option<(Move, i32)> {
    let mut rng = rand::rng();
    let mut moves = candidates(board, me);
    if moves.is_empty() {
        return None;
    }
    moves.shuffle(&mut rng);

    match level {
        Difficulty::Easy => {
            // Avoid repeating past positions or back-and-forth jumps unless no other move exists.
            let non_rep: Vec<Move> = moves.iter().copied().filter(|&m| !repeats_history(board, me, m, history)).collect();
            let pool = if non_rep.is_empty() { &moves } else { &non_rep };
            if rng.random_bool(0.3) {
                let m = pool[0];
                return Some((m, 0));
            }
            let m = pool.iter().copied().max_by_key(|&m| gain(board, me, m) * 10 + rng.random_range(0..30))?;
            Some((m, 0))
        }
        Difficulty::Medium => {
            order(board, me, &mut moves);
            let mut search = Search::with_mode(None, EvalMode::Light);
            search.set_history(history);
            // Small jitter among near-equal moves so Medium plays varied games.
            let mut best: Option<(Move, i32, i32)> = None;
            for &m in &moves {
                let mut b = board.clone();
                let conv = b.apply(me, m);
                let mut s = -search.negamax_board(&b, me.other(), 1, -INF, INF);
                let rep = history.boards.contains(&board_hash(&b))
                    || (board.is_jump(m) && conv.is_empty() && history.my_sides.contains(&side_hash(&b, me)));
                if rep && s > -WIN {
                    s -= 300;
                }
                let jittered = s + rng.random_range(-20..=20);
                if best.is_none_or(|(_, _, bj)| jittered > bj) {
                    best = Some((m, s, jittered));
                }
            }
            best.map(|(m, s, _)| (m, s))
        }
        Difficulty::Hard => {
            order(board, me, &mut moves);
            let mut search = Search::new(Some(Instant::now() + HARD_BUDGET));
            search.set_history(history);
            let mut best = moves[0];
            let mut best_score = 0;
            for depth in 1..=HARD_MAX_DEPTH {
                let Some((m, score)) = search.root(board, me, &moves, depth) else { break };
                best = m;
                best_score = score;
                let pos = moves.iter().position(|&x| x == m).unwrap_or(0);
                moves[..=pos].rotate_right(1);
                if score.abs() >= WIN {
                    break; // outcome is forced
                }
            }
            Some((best, best_score))
        }
    }
}

fn repeats_history(board: &Board, me: Player, m: Move, history: &History) -> bool {
    if history.boards.is_empty() && history.my_sides.is_empty() {
        return false;
    }
    let mut b = board.clone();
    let conv = b.apply(me, m);
    history.boards.contains(&board_hash(&b))
        || (board.is_jump(m) && conv.is_empty() && history.my_sides.contains(&side_hash(&b, me)))
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

/* ------------------------------------------------------------------ bitboard geometry */

#[derive(Clone)]
struct Geom {
    w: usize,
    h: usize,
    board_mask: u64,
    playable: u64,
    total_playable: i32,
    not_left: u64,
    not_right: u64,
    not_left2: u64,
    not_right2: u64,
    inner_mask: u64,
    mid_mask: u64,
    adj1: [u64; 64],
    jump2: [u64; 64],
    zobrist: [[u64; 64]; 2],
    zobrist_side: u64,
}

impl Geom {
    fn from_board(board: &Board) -> Option<Geom> {
        let n = board.w * board.h;
        if n == 0 || n > 64 {
            return None;
        }
        let w = board.w;
        let h = board.h;
        let board_mask = if n == 64 { u64::MAX } else { (1u64 << n) - 1 };
        let mut playable = 0u64;
        let mut not_left = 0u64;
        let mut not_right = 0u64;
        let mut not_left2 = 0u64;
        let mut not_right2 = 0u64;
        let mut inner_mask = 0u64;
        let mut mid_mask = 0u64;
        let mut adj1 = [0u64; 64];
        let mut jump2 = [0u64; 64];

        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                let bit = 1u64 << i;
                if board.cells[i] != Cell::Blocked {
                    playable |= bit;
                }
                if x > 0 {
                    not_left |= bit;
                }
                if x + 1 < w {
                    not_right |= bit;
                }
                if x > 1 {
                    not_left2 |= bit;
                }
                if x + 2 < w {
                    not_right2 |= bit;
                }
                if x >= 1 && x + 1 < w && y >= 1 && y + 1 < h {
                    mid_mask |= bit;
                }
                if x >= 2 && x + 2 < w && y >= 2 && y + 2 < h {
                    inner_mask |= bit;
                }
                for dy in -1i32..=1 {
                    for dx in -1i32..=1 {
                        if dx == 0 && dy == 0 {
                            continue;
                        }
                        let nx = x as i32 + dx;
                        let ny = y as i32 + dy;
                        if nx >= 0 && ny >= 0 && nx < w as i32 && ny < h as i32 {
                            adj1[i] |= 1u64 << (ny as usize * w + nx as usize);
                        }
                    }
                }
                for &(dx, dy) in &[(2i32, 0), (-2, 0), (0, 2), (0, -2)] {
                    let nx = x as i32 + dx;
                    let ny = y as i32 + dy;
                    if nx >= 0 && ny >= 0 && nx < w as i32 && ny < h as i32 {
                        jump2[i] |= 1u64 << (ny as usize * w + nx as usize);
                    }
                }
            }
        }

        let zobrist = [
            std::array::from_fn(|i| splitmix64(i as u64 * 2 + 1)),
            std::array::from_fn(|i| splitmix64(i as u64 * 2 + 2)),
        ];
        let zobrist_side = splitmix64(0x517c_c1b7_2722_0a95);

        Some(Geom {
            w,
            h,
            board_mask,
            playable,
            total_playable: playable.count_ones() as i32,
            not_left,
            not_right,
            not_left2,
            not_right2,
            inner_mask: inner_mask & playable,
            mid_mask: mid_mask & playable,
            adj1,
            jump2,
            zobrist,
            zobrist_side,
        })
    }

    #[inline]
    fn bitboards(&self, board: &Board, me: Player) -> (u64, u64) {
        let mut my = 0u64;
        let mut op = 0u64;
        for (i, &c) in board.cells.iter().enumerate() {
            match c {
                Cell::Piece(p) if p == me => my |= 1u64 << i,
                Cell::Piece(_) => op |= 1u64 << i,
                _ => {}
            }
        }
        (my, op)
    }

    #[inline]
    fn hash(&self, my: u64, op: u64, my_is_green: bool) -> u64 {
        let (g, r) = if my_is_green { (my, op) } else { (op, my) };
        let mut h = if my_is_green { 0 } else { self.zobrist_side };
        let mut b = g;
        while b != 0 {
            let i = b.trailing_zeros() as usize;
            h ^= self.zobrist[0][i];
            b &= b - 1;
        }
        let mut b = r;
        while b != 0 {
            let i = b.trailing_zeros() as usize;
            h ^= self.zobrist[1][i];
            b &= b - 1;
        }
        h
    }

    /// All 8 king-step neighbours of every set bit in `bb`.
    #[inline]
    fn dilate_adj1(&self, bb: u64) -> u64 {
        let l = (bb & self.not_left) >> 1;
        let r = (bb & self.not_right) << 1;
        let horiz = bb | l | r;
        let (u, d) = if self.h > 1 { (horiz >> self.w, horiz << self.w) } else { (0, 0) };
        (l | r | u | d) & self.board_mask
    }

    /// All 4 straight 2-step jump destinations of every set bit in `bb`.
    #[inline]
    fn dilate_jump2(&self, bb: u64) -> u64 {
        let l2 = (bb & self.not_left2) >> 2;
        let r2 = (bb & self.not_right2) << 2;
        let (u2, d2) = if self.h > 2 {
            let w2 = self.w * 2;
            (bb >> w2, bb << w2)
        } else {
            (0, 0)
        };
        (l2 | r2 | u2 | d2) & self.board_mask
    }

    #[inline]
    fn step(&self, bb: u64) -> u64 {
        self.dilate_adj1(bb) | self.dilate_jump2(bb)
    }
}

/// True when `my` is sealed off from `op` with no possible way to reach `op` and
/// fewer reachable cells than `op`'s guaranteed cells.
fn is_mathematically_sealed_loss(g: &Geom, my: u64, op: u64) -> bool {
    let empty = g.playable & !(my | op);
    if empty.count_ones() <= 1 {
        return false;
    }
    let mut m_empty = g.step(my) & empty;
    loop {
        let next = g.step(m_empty) & empty & !m_empty;
        if next == 0 {
            break;
        }
        m_empty |= next;
    }
    let my_zone = my | m_empty;
    // If any square in our reachable zone is adjacent to an opponent piece,
    // we can still land beside it and convert enemy pieces.
    if (g.dilate_adj1(my_zone) & op) != 0 {
        return false;
    }
    let max_my = my_zone.count_ones();
    if max_my < op.count_ones() {
        return true;
    }
    if (g.dilate_jump2(my_zone) & op) != 0 {
        return false;
    }
    let mut o_empty = g.step(op) & empty;
    loop {
        let next = g.step(o_empty) & empty & !o_empty;
        if next == 0 {
            break;
        }
        o_empty |= next;
    }
    (m_empty & o_empty) == 0 && max_my < (op | o_empty).count_ones()
}

/* ------------------------------------------------------------------ evaluation */

#[inline]
fn stuck_score(my_cnt: i32, op_cnt: i32, empty_cnt: i32, ply: usize) -> i32 {
    let diff = my_cnt - (op_cnt + empty_cnt);
    let ply_pen = (ply as i32) * 20;
    if diff > 0 {
        WIN_SCORE + diff * 10 - ply_pen
    } else if diff < 0 {
        -WIN_SCORE + diff * 10 + ply_pen
    } else {
        0
    }
}

/// Strategic evaluation in centi-cells (`100` = 1.00 cell) for the side to move (`my`).
fn evaluate(g: &Geom, my: u64, op: u64, mode: EvalMode) -> i32 {
    let empty = g.playable & !(my | op);
    let my_cnt = my.count_ones() as i32;
    let op_cnt = op.count_ones() as i32;
    let empty_cnt = empty.count_ones() as i32;

    let my_adj1 = g.dilate_adj1(my) & empty;
    let op_adj1 = g.dilate_adj1(op) & empty;
    let my_r1 = (my_adj1 | g.dilate_jump2(my)) & empty;
    let op_r1 = (op_adj1 | g.dilate_jump2(op)) & empty;

    // Simultaneous multi-source bitboard BFS for Voronoi territory & exclusive pockets.
    let mut m_seen = my_r1;
    let mut o_seen = op_r1;
    let mut m_front = my_r1;
    let mut o_front = op_r1;
    let mut m_pend = my_r1 & !op_r1;
    let mut o_pend = op_r1 & !my_r1;
    let mut m_lead1 = 0u64;
    let mut o_lead1 = 0u64;
    let mut m_lead2 = 0u64;
    let mut o_lead2 = 0u64;

    while (m_front | o_front) != 0 {
        let m_next = if m_front != 0 { g.step(m_front) & empty & !m_seen } else { 0 };
        let o_next = if o_front != 0 { g.step(o_front) & empty & !o_seen } else { 0 };

        m_lead1 |= m_pend & o_next;
        m_lead2 |= m_pend & !o_next;
        o_lead1 |= o_pend & m_next;
        o_lead2 |= o_pend & !m_next;

        m_pend = m_next & !o_seen & !o_next;
        o_pend = o_next & !m_seen & !m_next;

        m_seen |= m_next;
        o_seen |= o_next;
        m_front = m_next;
        o_front = o_next;
    }

    let m_only = m_seen & !o_seen;
    let o_only = o_seen & !m_seen;
    m_lead2 &= !m_only;
    o_lead2 &= !o_only;

    let only_diff = m_only.count_ones() as i32 - o_only.count_ones() as i32;
    let lead2_diff = m_lead2.count_ones() as i32 - o_lead2.count_ones() as i32;
    let lead1_diff = m_lead1.count_ones() as i32 - o_lead1.count_ones() as i32;

    let my_copy_mob = my_adj1.count_ones() as i32;
    let op_copy_mob = op_adj1.count_ones() as i32;
    let my_mob = my_r1.count_ones() as i32;
    let op_mob = op_r1.count_ones() as i32;

    let empty_adj = g.dilate_adj1(empty);
    let my_interior = (my & !empty_adj).count_ones() as i32;
    let op_interior = (op & !empty_adj).count_ones() as i32;

    let mut score = (my_cnt - op_cnt) * SCALE;

    match mode {
        EvalMode::Light => {
            score += only_diff * 35 + lead2_diff * 18 + lead1_diff * 9;
            score += (my_copy_mob - op_copy_mob) * 4 + (my_mob - op_mob) * 2;
            score += (my_interior - op_interior) * 6;
            score += (empty_cnt * 14).min(95);
        }
        EvalMode::Full => {
            // Territory / space control.
            score += only_diff * 68 + lead2_diff * 34 + lead1_diff * 17;

            // Mobility and strangulation (driving opponent toward having no moves).
            score += (my_copy_mob - op_copy_mob) * 9 + (my_mob - op_mob) * 4;
            if op_mob == 0 {
                score += empty_cnt * 60 + 250;
            } else if op_mob == 1 && empty_cnt >= 4 {
                score += empty_cnt * 16 + 70;
            } else if op_mob == 2 && empty_cnt >= 6 {
                score += empty_cnt * 7 + 25;
            }
            if my_mob == 1 && empty_cnt >= 4 {
                score -= empty_cnt * 16 + 70;
            } else if my_mob == 2 && empty_cnt >= 6 {
                score -= empty_cnt * 7 + 25;
            }

            // Piece safety: interior anchors (zero empty neighbours) & unreachable pieces.
            let my_secure = (my & !g.dilate_adj1(o_seen)).count_ones() as i32;
            let op_secure = (op & !g.dilate_adj1(m_seen)).count_ones() as i32;
            score += (my_interior - op_interior) * 14 + (my_secure - op_secure) * 10;

            // Immediate tactical exposure.
            let my_exposed = (my & g.dilate_adj1(op_r1)).count_ones() as i32;
            let op_exposed = (op & g.dilate_adj1(my_r1)).count_ones() as i32;
            score += (op_exposed - my_exposed) * 8;

            // Subtle early-game centrality tiebreaker.
            if empty_cnt > 8 {
                let my_c = (my & g.inner_mask).count_ones() as i32 * 4 + (my & g.mid_mask).count_ones() as i32 * 2;
                let op_c = (op & g.inner_mask).count_ones() as i32 * 4 + (op & g.mid_mask).count_ones() as i32 * 2;
                score += (my_c - op_c) * empty_cnt / g.total_playable.max(1);
            }

            // Side-to-move tempo: ~half an average move's material + Voronoi gain,
            // accurately valuing initiative and cancelling odd/even horizon zigzag.
            score += (empty_cnt * 18).min(130);
        }
    }

    score.clamp(-(WIN - 100), WIN - 100)
}

/* ------------------------------------------------------------------ search */

/// Compact move inside bitboard search: copy uses `from == 64` (target-only), jump uses `0..64`.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
struct PackedMove(u16);

impl PackedMove {
    const NONE: PackedMove = PackedMove(0);

    #[inline]
    fn copy(to: u8) -> PackedMove {
        PackedMove(((64u16 << 6) | (to as u16)) + 1)
    }

    #[inline]
    fn jump(from: u8, to: u8) -> PackedMove {
        PackedMove((((from as u16) << 6) | (to as u16)) + 1)
    }

    #[inline]
    fn is_none(self) -> bool {
        self.0 == 0
    }

    #[inline]
    fn unpack(self) -> (bool, usize, usize) {
        let v = self.0 - 1;
        let from = (v >> 6) as usize;
        let to = (v & 63) as usize;
        (from < 64, from, to)
    }
}

const FLAG_NONE: u8 = 0;
const FLAG_EXACT: u8 = 1;
const FLAG_LOWER: u8 = 2;
const FLAG_UPPER: u8 = 3;

#[derive(Clone, Copy, Default)]
struct TTEntry {
    key: u64,
    score: i32,
    best: PackedMove,
    depth: u8,
    flag: u8,
}

#[inline]
fn tt_store_score(score: i32, ply: usize) -> i32 {
    if score >= WIN {
        score + (ply as i32) * 20
    } else if score <= -WIN {
        score - (ply as i32) * 20
    } else {
        score
    }
}

#[inline]
fn tt_load_score(score: i32, ply: usize) -> i32 {
    if score >= WIN {
        score - (ply as i32) * 20
    } else if score <= -WIN {
        score + (ply as i32) * 20
    } else {
        score
    }
}

pub(crate) struct Search {
    deadline: Option<Instant>,
    mode: EvalMode,
    nodes: u64,
    aborted: bool,
    tt: Vec<TTEntry>,
    killers: [[PackedMove; 2]; MAX_PLY],
    history: History,
}

impl Search {
    pub(crate) fn new(deadline: Option<Instant>) -> Search {
        Self::with_mode(deadline, EvalMode::Full)
    }

    pub(crate) fn with_mode(deadline: Option<Instant>, mode: EvalMode) -> Search {
        Search {
            deadline,
            mode,
            nodes: 0,
            aborted: false,
            tt: vec![TTEntry::default(); TT_SIZE],
            killers: [[PackedMove::NONE; 2]; MAX_PLY],
            history: History::default(),
        }
    }

    pub(crate) fn set_history(&mut self, history: &History) {
        self.history = history.clone();
    }

    /// Best move at the root, or `None` if time ran out before finishing this depth.
    pub(crate) fn root(&mut self, board: &Board, me: Player, moves: &[Move], depth: u32) -> Option<(Move, i32)> {
        if let Some(g) = Geom::from_board(board) {
            return self.root_fast(&g, board, me, moves, depth);
        }
        self.root_fallback(board, me, moves, depth)
    }

    /// Searches a board position after a move (used by analysis).
    pub(crate) fn negamax_board(&mut self, board: &Board, p: Player, depth: u32, alpha: i32, beta: i32) -> i32 {
        if let Some(g) = Geom::from_board(board) {
            let (my, op) = g.bitboards(board, p);
            return self.pvs(&g, my, op, p == Player::Green, depth, 1, alpha, beta);
        }
        self.negamax_fallback(board, p, depth, 1, alpha, beta)
    }

    fn root_fast(&mut self, g: &Geom, board: &Board, me: Player, moves: &[Move], depth: u32) -> Option<(Move, i32)> {
        let (my, op) = g.bitboards(board, me);
        let my_is_green = me == Player::Green;
        let mut alpha = -INF;
        let mut best: Option<(Move, i32, i32)> = None;

        for (idx, &m) in moves.iter().enumerate() {
            let is_jump = board.is_jump(m);
            let cap = g.adj1[m.to] & op;
            let next_my = if is_jump {
                (my ^ (1u64 << m.from)) | (1u64 << m.to) | cap
            } else {
                my | (1u64 << m.to) | cap
            };
            let next_op = op & !cap;

            // Penalize repeating an earlier board state or jumping back into a previous own-piece
            // shape so the AI makes progress instead of oscillating back and forth.
            let rep_pen = if self.is_repeated_fast(g, next_my, next_op, my_is_green, is_jump && cap == 0) {
                if is_jump { 350 } else { 200 }
            } else {
                0
            };

            let raw = if idx == 0 || rep_pen > 0 {
                -self.pvs(g, next_op, next_my, !my_is_green, depth - 1, 1, -INF, -alpha)
            } else {
                let s = -self.pvs(g, next_op, next_my, !my_is_green, depth - 1, 1, -alpha - 1, -alpha);
                if s > alpha && !self.aborted {
                    -self.pvs(g, next_op, next_my, !my_is_green, depth - 1, 1, -INF, -alpha)
                } else {
                    s
                }
            };
            if self.aborted {
                return None;
            }
            let adj = if rep_pen > 0 && raw.abs() < WIN { raw - rep_pen } else { raw };
            if best.is_none_or(|(_, best_adj, _)| adj > best_adj) {
                best = Some((m, adj, raw));
            }
            alpha = alpha.max(adj);
        }
        best.map(|(m, _, raw)| (m, raw))
    }

    fn is_repeated_fast(&self, g: &Geom, my: u64, op: u64, my_is_green: bool, quiet_jump: bool) -> bool {
        if quiet_jump && !self.history.my_sides.is_empty() {
            let mut sh = 0x243f_6a88_85a3_08d3u64;
            let mut b = my;
            while b != 0 {
                let i = b.trailing_zeros() as u64;
                sh ^= splitmix64(i.wrapping_mul(4).wrapping_add(1));
                b &= b - 1;
            }
            if self.history.my_sides.contains(&sh) {
                return true;
            }
        }
        if self.history.boards.is_empty() {
            return false;
        }
        // Compute the same board_hash as `board_hash(&Board)` directly from bitboards.
        let (gr, rd) = if my_is_green { (my, op) } else { (op, my) };
        let walls = g.board_mask & !g.playable;
        let mut h = 0x9e37_79b9_7f4a_7c15u64 ^ ((g.w as u64) << 8) ^ (g.h as u64);
        let mut b = gr;
        while b != 0 {
            let i = b.trailing_zeros() as u64;
            h ^= splitmix64(i.wrapping_mul(4).wrapping_add(1));
            b &= b - 1;
        }
        let mut b = rd;
        while b != 0 {
            let i = b.trailing_zeros() as u64;
            h ^= splitmix64(i.wrapping_mul(4).wrapping_add(2));
            b &= b - 1;
        }
        let mut b = walls;
        while b != 0 {
            let i = b.trailing_zeros() as u64;
            h ^= splitmix64(i.wrapping_mul(4).wrapping_add(3));
            b &= b - 1;
        }
        self.history.boards.contains(&h)
    }

    #[allow(clippy::too_many_arguments)]
    fn pvs(
        &mut self,
        g: &Geom,
        my: u64,
        op: u64,
        my_is_green: bool,
        depth: u32,
        ply: usize,
        mut alpha: i32,
        mut beta: i32,
    ) -> i32 {
        self.nodes += 1;
        if self.nodes & 2047 == 0 && self.deadline.is_some_and(|d| Instant::now() >= d) {
            self.aborted = true;
        }
        if self.aborted {
            return 0;
        }

        let empty = g.playable & !(my | op);
        let copy_targets = g.dilate_adj1(my) & empty;
        let has_any = copy_targets != 0 || (g.dilate_jump2(my) & empty) != 0;
        if !has_any {
            return stuck_score(my.count_ones() as i32, op.count_ones() as i32, empty.count_ones() as i32, ply);
        }
        if depth == 0 {
            return if self.mode == EvalMode::Full {
                self.qsearch(g, my, op, 2, ply, alpha, beta)
            } else {
                evaluate(g, my, op, self.mode)
            };
        }

        let orig_alpha = alpha;
        let key = g.hash(my, op, my_is_green);
        let tt_idx = (key as usize) & TT_MASK;
        let entry = self.tt[tt_idx];
        let mut tt_move = PackedMove::NONE;
        if entry.flag != FLAG_NONE && entry.key == key {
            tt_move = entry.best;
            if entry.depth as u32 >= depth {
                let s = tt_load_score(entry.score, ply);
                match entry.flag {
                    FLAG_EXACT => return s,
                    FLAG_LOWER => alpha = alpha.max(s),
                    FLAG_UPPER => beta = beta.min(s),
                    _ => {}
                }
                if alpha >= beta {
                    return s;
                }
            }
        }

        // Generate all legal moves into a small stack buffer (max ~64 copies + ~128 jumps).
        let mut moves = [(PackedMove::NONE, 0i32); 256];
        let mut count = 0usize;
        let k0 = if ply < MAX_PLY { self.killers[ply][0] } else { PackedMove::NONE };
        let k1 = if ply < MAX_PLY { self.killers[ply][1] } else { PackedMove::NONE };

        let mut ct = copy_targets;
        while ct != 0 {
            let to = ct.trailing_zeros() as usize;
            ct &= ct - 1;
            let pm = PackedMove::copy(to as u8);
            let caps = (g.adj1[to] & op).count_ones() as i32;
            let prio = if pm == tt_move {
                1_000_000
            } else if pm == k0 {
                500_000
            } else if pm == k1 {
                499_000
            } else {
                200 + caps * 20 + i32::from((g.inner_mask >> to) & 1 != 0) * 3
            };
            moves[count] = (pm, prio);
            count += 1;
        }

        let mut pieces = my;
        while pieces != 0 {
            let from = pieces.trailing_zeros() as usize;
            pieces &= pieces - 1;
            let mut jt = g.jump2[from] & empty;
            while jt != 0 {
                let to = jt.trailing_zeros() as usize;
                jt &= jt - 1;
                let pm = PackedMove::jump(from as u8, to as u8);
                let caps = (g.adj1[to] & op).count_ones() as i32;
                let prio = if pm == tt_move {
                    1_000_000
                } else if pm == k0 {
                    500_000
                } else if pm == k1 {
                    499_000
                } else if caps >= 2 {
                    200 + caps * 20 - 10
                } else {
                    100 + caps * 20 + i32::from((g.inner_mask >> to) & 1 != 0) * 3
                };
                moves[count] = (pm, prio);
                count += 1;
            }
        }

        // Incremental selection sort: most cutoffs happen in the first 1-3 moves!
        let mut best_score = -INF;
        let mut best_move = PackedMove::NONE;

        for i in 0..count {
            let mut best_idx = i;
            let mut best_prio = moves[i].1;
            for (j, &(_, prio)) in moves.iter().enumerate().take(count).skip(i + 1) {
                if prio > best_prio {
                    best_prio = prio;
                    best_idx = j;
                }
            }
            if best_idx != i {
                moves.swap(i, best_idx);
            }
            let pm = moves[i].0;
            let (is_jump, from, to) = pm.unpack();
            let cap = g.adj1[to] & op;
            let next_my = if is_jump {
                (my ^ (1u64 << from)) | (1u64 << to) | cap
            } else {
                my | (1u64 << to) | cap
            };
            let next_op = op & !cap;

            let v = if i == 0 {
                -self.pvs(g, next_op, next_my, !my_is_green, depth - 1, ply + 1, -beta, -alpha)
            } else {
                let s = -self.pvs(g, next_op, next_my, !my_is_green, depth - 1, ply + 1, -alpha - 1, -alpha);
                if s > alpha && s < beta && !self.aborted {
                    -self.pvs(g, next_op, next_my, !my_is_green, depth - 1, ply + 1, -beta, -alpha)
                } else {
                    s
                }
            };
            if self.aborted {
                return 0;
            }
            if v > best_score {
                best_score = v;
                best_move = pm;
            }
            if v > alpha {
                alpha = v;
                if alpha >= beta {
                    if ply < MAX_PLY && self.killers[ply][0] != pm {
                        self.killers[ply][1] = self.killers[ply][0];
                        self.killers[ply][0] = pm;
                    }
                    break;
                }
            }
        }

        let flag = if best_score <= orig_alpha {
            FLAG_UPPER
        } else if best_score >= beta {
            FLAG_LOWER
        } else {
            FLAG_EXACT
        };
        if entry.flag == FLAG_NONE || entry.key != key || entry.depth as u32 <= depth {
            self.tt[tt_idx] = TTEntry {
                key,
                score: tt_store_score(best_score, ply),
                best: if !best_move.is_none() { best_move } else { entry.best },
                depth: depth.min(255) as u8,
                flag,
            };
        }

        best_score
    }

    /// Bounded tactical quiescence search for large captures (net material gain >= 5),
    /// eliminating horizon-capture blunders and smoothing odd/even parity swings.
    #[allow(clippy::too_many_arguments)]
    fn qsearch(&mut self, g: &Geom, my: u64, op: u64, qdepth: u8, ply: usize, mut alpha: i32, beta: i32) -> i32 {
        self.nodes += 1;
        let empty = g.playable & !(my | op);
        let copy_targets = g.dilate_adj1(my) & empty;
        let has_any = copy_targets != 0 || (g.dilate_jump2(my) & empty) != 0;
        if !has_any {
            return stuck_score(my.count_ones() as i32, op.count_ones() as i32, empty.count_ones() as i32, ply);
        }
        let stand_pat = evaluate(g, my, op, self.mode);
        if qdepth == 0 || stand_pat >= beta {
            return stand_pat;
        }
        if stand_pat > alpha {
            alpha = stand_pat;
        }

        // Pick up to the 2 biggest tactical capture moves (net gain >= 5: copy x2+ or jump x3+).
        let mut best_caps: [(PackedMove, i32); 2] = [(PackedMove::NONE, 0); 2];
        let mut insert_cap = |pm: PackedMove, net: i32| {
            if net > best_caps[0].1 {
                best_caps[1] = best_caps[0];
                best_caps[0] = (pm, net);
            } else if net > best_caps[1].1 {
                best_caps[1] = (pm, net);
            }
        };

        let mut ct = copy_targets;
        while ct != 0 {
            let to = ct.trailing_zeros() as usize;
            ct &= ct - 1;
            let caps = (g.adj1[to] & op).count_ones() as i32;
            if caps >= 2 {
                insert_cap(PackedMove::copy(to as u8), caps * 2 + 1);
            }
        }
        let mut pieces = my;
        while pieces != 0 {
            let from = pieces.trailing_zeros() as usize;
            pieces &= pieces - 1;
            let mut jt = g.jump2[from] & empty;
            while jt != 0 {
                let to = jt.trailing_zeros() as usize;
                jt &= jt - 1;
                let caps = (g.adj1[to] & op).count_ones() as i32;
                if caps >= 3 {
                    insert_cap(PackedMove::jump(from as u8, to as u8), caps * 2);
                }
            }
        }

        let mut best = stand_pat;
        for &(pm, net) in &best_caps {
            if pm.is_none() || net < 5 {
                break;
            }
            let (is_jump, from, to) = pm.unpack();
            let cap = g.adj1[to] & op;
            let next_my = if is_jump {
                (my ^ (1u64 << from)) | (1u64 << to) | cap
            } else {
                my | (1u64 << to) | cap
            };
            let next_op = op & !cap;
            let v = -self.qsearch(g, next_op, next_my, qdepth - 1, ply + 1, -beta, -alpha);
            if v > best {
                best = v;
                if v > alpha {
                    alpha = v;
                    if alpha >= beta {
                        break;
                    }
                }
            }
        }
        best
    }

    /* --- fallback for hypothetical boards larger than 64 cells --- */

    fn root_fallback(&mut self, board: &Board, me: Player, moves: &[Move], depth: u32) -> Option<(Move, i32)> {
        let mut alpha = -INF;
        let mut best: Option<(Move, i32)> = None;
        for &m in moves {
            let mut b = board.clone();
            b.apply(me, m);
            let v = -self.negamax_fallback(&b, me.other(), depth - 1, 1, -INF, -alpha);
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

    fn negamax_fallback(&mut self, board: &Board, p: Player, depth: u32, ply: usize, mut alpha: i32, beta: i32) -> i32 {
        self.nodes += 1;
        if self.nodes & 1023 == 0 && self.deadline.is_some_and(|d| Instant::now() >= d) {
            self.aborted = true;
        }
        if self.aborted {
            return 0;
        }
        if depth == 0 {
            return if board.has_moves(p) {
                (board.count(p) as i32 - board.count(p.other()) as i32) * SCALE
            } else {
                stuck_score(board.count(p) as i32, board.count(p.other()) as i32, board.empties() as i32, ply)
            };
        }
        let mut moves = candidates(board, p);
        if moves.is_empty() {
            return stuck_score(board.count(p) as i32, board.count(p.other()) as i32, board.empties() as i32, ply);
        }
        order(board, p, &mut moves);
        let mut best = -INF;
        for m in moves {
            let mut b = board.clone();
            b.apply(p, m);
            let v = -self.negamax_fallback(&b, p.other(), depth - 1, ply + 1, -beta, -alpha);
            if self.aborted {
                return 0;
            }
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
    fn expands_toward_open_space_not_corner() {
        // Green starts in bottom-right on an open 8x8 board; strategic eval should move toward center,
        // never into a dead corner/edge shuffle.
        let b = Board::parse(&[
            "R.......",
            "........",
            "........",
            "........",
            "........",
            "........",
            "........",
            ".......G",
        ]);
        let m = choose(&b, Player::Green, Difficulty::Hard).unwrap();
        let (x, y) = (m.to % 8, m.to / 8);
        assert!(x <= 6 && y <= 6, "expected inward move, got ({x}, {y})");
    }

    #[test]
    fn resigns_when_sealed_in_smaller_pocket() {
        // Green is sealed in a 2-cell pocket behind 2-cell-thick walls; Red already owns 10 cells.
        let b = Board::parse(&[
            "RRRRR...",
            "RRRRR...",
            "........",
            ".....###",
            ".....###",
            ".....##.",
            ".....##G",
            ".....###",
        ]);
        let hist = History::default();
        for level in [Difficulty::Easy, Difficulty::Medium, Difficulty::Hard] {
            let out = decide(&b, Player::Green, level, 20, &hist, 0).unwrap();
            assert_eq!(out.decision, Decision::Resign, "{level:?} should resign when sealed and lost");
        }
    }

    #[test]
    fn does_not_resign_when_jump_escape_exists() {
        // Only a 1-cell-thick wall above (7,5): Green can copy to (7,5) and jump out to (7,3)!
        let b = Board::parse(&[
            "RRRRR...",
            "RRRRR...",
            "........",
            "........",
            ".....###",
            ".....##.",
            ".....##G",
            ".....###",
        ]);
        let g = Geom::from_board(&b).unwrap();
        let (my, op) = g.bitboards(&b, Player::Green);
        assert!(!is_mathematically_sealed_loss(&g, my, op));
    }

    #[test]
    fn avoids_repeating_when_other_moves_exist() {
        // Green can jump back to (0,0) (a repeated board) or expand toward open space.
        let prev = Board::parse(&[
            "G.......",
            "........",
            ".......R",
        ]);
        let cur = Board::parse(&[
            "..G.....",
            "........",
            ".......R",
        ]);
        let hist = History {
            boards: vec![board_hash(&prev)],
            my_sides: vec![side_hash(&prev, Player::Green)],
        };
        let out = decide(&cur, Player::Green, Difficulty::Hard, 4, &hist, 0).unwrap();
        match out.decision {
            Decision::Move(m) => assert_ne!(m.to, 0, "should not jump right back to repeated square"),
            Decision::Resign => panic!("should not resign in an even game"),
        }
    }

    #[test]
    fn resigns_when_looping_while_human_fills_elsewhere() {
        // Green jumped (6,7) -> (6,5), Red copied on the far top-left (changing the full-board
        // hash), and Green is now hopelessly crushed with its only move being a zero-capture jump
        // back to (6,7), repeating Green's own piece shape.
        let prev = Board::parse(&[
            "RRRRR...",
            "RRRRR...",
            "RRRRR...",
            "RRRRRRRR",
            "RRRRRRRR",
            "RRRRRR.#",
            "RRRRR###",
            "RRRRR#G#",
        ]);
        let cur = Board::parse(&[
            "RRRRRR..", // Red added a cell here so full board_hash differs!
            "RRRRR...",
            "RRRRR...",
            "RRRRRRRR",
            "RRRRRRRR",
            "RRRRRRG#",
            "RRRRR###",
            "RRRRR#.#",
        ]);
        let hist = History {
            boards: vec![board_hash(&prev)],
            my_sides: vec![side_hash(&prev, Player::Green)],
        };
        for level in [Difficulty::Easy, Difficulty::Medium, Difficulty::Hard] {
            let out = decide(&cur, Player::Green, level, 30, &hist, 0).unwrap();
            assert_eq!(out.decision, Decision::Resign, "{level:?} should resign instead of jumping back and forth");
        }
    }

    #[test]
    fn resigns_after_sustained_hopelessness() {
        // Green is overwhelmingly behind across the board (not yet forced-lost in 4 plies),
        // so after 2 hopeless turns the 3rd turn triggers resignation.
        let b = Board::parse(&[
            "RRRRRRRR",
            "RRRRRRRR",
            "RRRRRRRR",
            "RRRRRR..",
            "........",
            "........",
            "......GG",
            "......GG",
        ]);
        let hist = History::default();
        let out = decide(&b, Player::Green, Difficulty::Hard, 28, &hist, 2).unwrap();
        assert_eq!(out.decision, Decision::Resign);
    }

    #[test]
    fn does_not_resign_on_last_empty_cell() {
        // Only 1 empty cell left: play it to finish the game cleanly rather than resigning.
        let b = Board::parse(&[
            "RRRRRRRR",
            "RRRRRRRR",
            "RRRRRRRR",
            "RRRRRRRR",
            "RRRRRRRR",
            "RRRRRRRR",
            "RRRRRRRR",
            "RRRRRRG.",
        ]);
        let out = decide(&b, Player::Green, Difficulty::Hard, 60, &History::default(), 5).unwrap();
        assert_eq!(out.decision, Decision::Move(Move { from: 62, to: 63 }));
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

    #[test]
    fn strategic_eval_values_sealed_territory() {
        // Equal material (4 vs 4), but Green sealed off a huge open room while Red is cramped.
        let b = Board::parse(&[
            "RRRR##..",
            "....##..",
            "####GG..",
            "....GG..",
            "........",
            "........",
            "........",
            "........",
        ]);
        let g = Geom::from_board(&b).unwrap();
        let (my, op) = g.bitboards(&b, Player::Green);
        let score = evaluate(&g, my, op, EvalMode::Full);
        assert!(score > 1000, "Green should be hugely ahead strategically, got {score}");
    }
}
