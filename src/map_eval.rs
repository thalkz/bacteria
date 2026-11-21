//! Board quality evaluator: measures how interesting, strategically diverse,
//! visually simple, and distinct a set of boards is.
//!
//! Run from the CLI:
//!   cargo run --release -- --eval-maps [--json] [custom_map_file...]
//!
//! Combines:
//! 1. **Complex Interaction (35%)**: non-greedy tactical depth, game tension & lead
//!    swings, combat conversions, wall-jump bridges, and chokepoint geometry.
//! 2. **Strategical Diversity (30%)**: viable opening plans & spatial spread,
//!    midgame trajectory divergence across diverse strong games, copy/jump balance,
//!    multi-lane connectivity, and first-mover fairness.
//! 3. **Visual Simplicity (20%)**: low wall fragmentation (few components, few
//!    isolated 1×1 dots), orthogonal linearity & compactness, dihedral symmetry,
//!    and positive spatial order.
//! 4. **Distinctness (15%)**: dihedral-invariant layout distance (walls + starts
//!    across all 8 rotations/reflections) and gameplay-fingerprint distance from
//!    every other board in the pool.

use crate::ai::{self, Decision, Difficulty, History, Search, WIN};
use crate::game::{Board, Cell, Game, Move, Player};
use crate::maps;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde::Serialize;
use std::collections::VecDeque;
use std::fs;

const SIM_GAMES: usize = 16;
const SIM_DEPTH: u32 = 4;
const OPEN_DEPTH: u32 = 5;
const MAX_SIM_PLY: u32 = 140;

const AROUND: [(i32, i32); 8] = [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)];
const ORTHO: [(i32, i32); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];
const JUMPS: [(i32, i32); 4] = [(2, 0), (-2, 0), (0, 2), (0, -2)];

#[derive(Clone, Debug, Serialize)]
pub struct InteractionMetrics {
    pub score: f64,
    pub non_greedy_rate: f64,
    pub alive_fraction: f64,
    pub contested_ratio: f64,
    pub late_swings: f64,
    pub final_closeness: f64,
    pub contact_conversions: f64,
    pub late_conversions: f64,
    pub wall_jump_rate: f64,
    pub dead_ends: usize,
    pub mean_copy_degree: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct DiversityMetrics {
    pub score: f64,
    pub viable_openings: f64,
    pub opening_spread: f64,
    pub trajectory_divergence: f64,
    pub jump_rate: f64,
    pub start_eval_cells: f64,
    pub green_win_rate: f64,
    pub multi_lane_score: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct SimplicityMetrics {
    pub score: f64,
    pub walls: usize,
    pub playable: usize,
    pub components_4: usize,
    pub components_8: usize,
    pub isolated_dot_ratio: f64,
    pub ortho_line_ratio: f64,
    pub perimeter_per_wall: f64,
    pub wall_symmetries: usize,
    pub ortho_autocorr: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct DistinctnessMetrics {
    pub score: f64,
    pub nearest_neighbor: String,
    pub nearest_dist: f64,
    pub nearest_layout_dist: f64,
    pub nearest_play_dist: f64,
    pub mean_dist: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct BoardReport {
    pub name: String,
    pub overall: f64,
    pub avg_plies: f64,
    pub interaction: InteractionMetrics,
    pub diversity: DiversityMetrics,
    pub simplicity: SimplicityMetrics,
    pub distinctness: DistinctnessMetrics,
    #[serde(skip)]
    fingerprint: [f64; 10],
}

#[derive(Clone, Debug, Serialize)]
pub struct PoolReport {
    pub boards: Vec<BoardReport>,
    pub pairwise_distances: Vec<Vec<f64>>,
    pub pairwise_layout_distances: Vec<Vec<f64>>,
}

/// Evaluates a pool of named boards together (so each board's distinctness is measured
/// against the rest of the pool).
pub fn evaluate_pool(boards: &[(&str, Board)]) -> PoolReport {
    let mut reports: Vec<BoardReport> = boards
        .iter()
        .enumerate()
        .map(|(idx, (name, board))| evaluate_single(name, board, 0xBACC_0001u64.wrapping_add(idx as u64 * 997)))
        .collect();

    let n = boards.len();
    let mut layout_mat = vec![vec![0.0; n]; n];
    let mut play_mat = vec![vec![0.0; n]; n];
    let mut pair_mat = vec![vec![0.0; n]; n];

    // Standardize gameplay fingerprints across the pool (with sensible fallback scales
    // so even small pools of 2-6 boards produce stable distances).
    let scales = [0.10, 1.2, 0.12, 2.0, 0.12, 0.10, 15.0, 0.50, 0.25, 0.15];
    for i in 0..n {
        for j in (i + 1)..n {
            let ld = dihedral_layout_distance(&boards[i].1, &boards[j].1);
            let pd = fingerprint_distance(&reports[i].fingerprint, &reports[j].fingerprint, &scales);
            let combined = 0.62 * ld + 0.38 * pd;
            layout_mat[i][j] = ld;
            layout_mat[j][i] = ld;
            play_mat[i][j] = pd;
            play_mat[j][i] = pd;
            pair_mat[i][j] = combined;
            pair_mat[j][i] = combined;
        }
    }

    for i in 0..n {
        if n <= 1 {
            reports[i].distinctness = DistinctnessMetrics {
                score: 100.0,
                nearest_neighbor: "-".into(),
                nearest_dist: 1.0,
                nearest_layout_dist: 1.0,
                nearest_play_dist: 1.0,
                mean_dist: 1.0,
            };
        } else {
            let mut best_j = if i == 0 { 1 } else { 0 };
            let mut sum = 0.0;
            for j in 0..n {
                if i == j {
                    continue;
                }
                sum += pair_mat[i][j];
                if pair_mat[i][j] < pair_mat[i][best_j] {
                    best_j = j;
                }
            }
            let nearest = pair_mat[i][best_j];
            let mean = sum / (n - 1) as f64;
            let dist_score = scale_0_100(0.70 * nearest + 0.30 * mean, 0.12, 0.68);
            reports[i].distinctness = DistinctnessMetrics {
                score: dist_score,
                nearest_neighbor: boards[best_j].0.to_string(),
                nearest_dist: nearest,
                nearest_layout_dist: layout_mat[i][best_j],
                nearest_play_dist: play_mat[i][best_j],
                mean_dist: mean,
            };
        }
        reports[i].overall = 0.35 * reports[i].interaction.score
            + 0.30 * reports[i].diversity.score
            + 0.20 * reports[i].simplicity.score
            + 0.15 * reports[i].distinctness.score;
    }

    PoolReport { boards: reports, pairwise_distances: pair_mat, pairwise_layout_distances: layout_mat }
}

fn evaluate_single(name: &str, board: &Board, seed: u64) -> BoardReport {
    let simplicity = eval_simplicity(board);
    let topo = eval_topology(board);
    let opening = eval_openings(board);
    let sim = run_simulations(board, seed);

    // Pillar 1: Complex Interaction & Full-Game Vitality (0..100)
    // Heavily rewards sustained combat, late-game tension (avoiding early-decided "filling"),
    // tactical non-greedy play, and open flowing layouts without dead ends.
    let non_greedy_s = scale_0_100(sim.non_greedy_rate, 0.15, 0.60);
    let full_game_s = 0.45 * scale_0_100(sim.alive_fraction, 0.40, 0.90)
        + 0.30 * scale_0_100(sim.late_conversions, 0.20, 1.30)
        + 0.25 * scale_0_100(sim.late_swings, 0.4, 3.5);
    let combat_s = 0.45 * scale_0_100(sim.contact_conversions, 0.35, 1.50)
        + 0.30 * scale_0_100(sim.final_closeness, 0.50, 0.93)
        + 0.25 * scale_0_100(sim.wall_jump_rate, 0.01, 0.11);
    let flow_s = 0.45 * trapezoid_0_100(topo.mean_copy_degree, 3.8, 5.2, 6.5, 7.3)
        + 0.35 * (100.0 - scale_0_100(topo.dead_ends as f64, 0.0, 5.0))
        + 0.20 * (100.0 - scale_0_100(topo.cramped_ratio, 0.04, 0.32));
    let interaction_score = 0.20 * non_greedy_s + 0.35 * full_game_s + 0.25 * combat_s + 0.20 * flow_s;

    let interaction = InteractionMetrics {
        score: interaction_score,
        non_greedy_rate: sim.non_greedy_rate,
        alive_fraction: sim.alive_fraction,
        contested_ratio: sim.contested_ratio,
        late_swings: sim.late_swings,
        final_closeness: sim.final_closeness,
        contact_conversions: sim.contact_conversions,
        late_conversions: sim.late_conversions,
        wall_jump_rate: sim.wall_jump_rate,
        dead_ends: topo.dead_ends,
        mean_copy_degree: topo.mean_copy_degree,
    };

    // Pillar 2: Strategical Diversity (0..100)
    let open_plan_s = 0.55 * scale_0_100(opening.viable_count, 1.2, 4.8)
        + 0.45 * scale_0_100(opening.spatial_spread, 0.10, 0.45);
    let traj_s = scale_0_100(sim.trajectory_divergence, 0.22, 0.68);
    let jump_bal_s = trapezoid_0_100(sim.jump_rate, 0.02, 0.14, 0.34, 0.58);
    let eval_bias_s = 100.0 - scale_0_100((opening.start_eval_cells - 0.8).abs(), 0.2, 3.0);
    let win_bal_s = 100.0 - scale_0_100((sim.green_win_rate - 0.5).abs() * 2.0, 0.05, 0.75);
    let fairness_s = 0.45 * eval_bias_s + 0.55 * win_bal_s;
    let lane_s = scale_0_100(topo.multi_lane_score, 0.20, 0.90);
    let diversity_score =
        0.25 * open_plan_s + 0.30 * traj_s + 0.20 * jump_bal_s + 0.15 * fairness_s + 0.10 * lane_s;

    let diversity = DiversityMetrics {
        score: diversity_score,
        viable_openings: opening.viable_count,
        opening_spread: opening.spatial_spread,
        trajectory_divergence: sim.trajectory_divergence,
        jump_rate: sim.jump_rate,
        start_eval_cells: opening.start_eval_cells,
        green_win_rate: sim.green_win_rate,
        multi_lane_score: topo.multi_lane_score,
    };

    let total_cells = (board.w * board.h).max(1) as f64;
    let fingerprint = [
        simplicity.walls as f64 / total_cells,
        topo.mean_copy_degree,
        topo.wall_jump_bridge_ratio,
        topo.start_distance,
        topo.copy_vs_jump_gap,
        sim.jump_rate,
        sim.avg_plies,
        sim.late_conversions,
        opening.spatial_spread,
        sim.wall_jump_rate,
    ];

    BoardReport {
        name: name.to_string(),
        overall: 0.0,
        avg_plies: sim.avg_plies,
        interaction,
        diversity,
        simplicity,
        distinctness: DistinctnessMetrics {
            score: 0.0,
            nearest_neighbor: String::new(),
            nearest_dist: 0.0,
            nearest_layout_dist: 0.0,
            nearest_play_dist: 0.0,
            mean_dist: 0.0,
        },
        fingerprint,
    }
}

/* ------------------------------------------------------------------ visual simplicity */

fn eval_simplicity(board: &Board) -> SimplicityMetrics {
    let (w, h) = (board.w, board.h);
    let n = w * h;
    let is_wall = |x: i32, y: i32| -> bool {
        if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
            false
        } else {
            board.cells[y as usize * w + x as usize] == Cell::Blocked
        }
    };

    let walls = board.cells.iter().filter(|&&c| c == Cell::Blocked).count();
    let playable = n - walls;

    let comp4 = wall_components(board, &ORTHO);
    let comp8 = wall_components(board, &AROUND);

    let mut isolated_dots = 0usize;
    let mut in_ortho_line = 0usize;
    let mut exposed_edges = 0usize;

    for y in 0..h as i32 {
        for x in 0..w as i32 {
            if !is_wall(x, y) {
                continue;
            }
            let ortho_n = ORTHO.iter().filter(|&&(dx, dy)| is_wall(x + dx, y + dy)).count();
            if ortho_n == 0 {
                isolated_dots += 1;
            } else {
                in_ortho_line += 1;
            }
            exposed_edges += 4 - ortho_n;
        }
    }

    let isolated_dot_ratio = if walls > 0 { isolated_dots as f64 / walls as f64 } else { 0.0 };
    let ortho_line_ratio = if walls > 0 { in_ortho_line as f64 / walls as f64 } else { 0.0 };
    let perimeter_per_wall = if walls > 0 { exposed_edges as f64 / walls as f64 } else { 0.0 };

    // Orthogonal spatial autocorrelation (Moran-like: tendency of orthogonally adjacent cells
    // to share the same wall/non-wall state vs alternating like checkerboard scatter).
    let p_wall = walls as f64 / n.max(1) as f64;
    let var = p_wall * (1.0 - p_wall);
    let mut cov_sum = 0.0;
    let mut pairs = 0usize;
    for y in 0..h {
        for x in 0..w {
            let a = if board.cells[y * w + x] == Cell::Blocked { 1.0 } else { 0.0 } - p_wall;
            if x + 1 < w {
                let b = if board.cells[y * w + (x + 1)] == Cell::Blocked { 1.0 } else { 0.0 } - p_wall;
                cov_sum += a * b;
                pairs += 1;
            }
            if y + 1 < h {
                let b = if board.cells[(y + 1) * w + x] == Cell::Blocked { 1.0 } else { 0.0 } - p_wall;
                cov_sum += a * b;
                pairs += 1;
            }
        }
    }
    let ortho_autocorr = if var > 1e-6 && pairs > 0 { (cov_sum / pairs as f64) / var } else { 0.0 };

    // Count how many of the 8 dihedral symmetries preserve the wall pattern.
    let wall_symmetries = (0..8)
        .filter(|&t| {
            (0..n).all(|i| {
                let j = dihedral_idx(w, h, i, t);
                (board.cells[i] == Cell::Blocked) == (board.cells[j] == Cell::Blocked)
            })
        })
        .count();

    // Combine into Visual Simplicity (0..100).
    let cohesion_s = if walls == 0 {
        50.0 // blank board is simple, though featureless
    } else {
        let dot_s = 100.0 * (1.0 - isolated_dot_ratio);
        let c4_s = 100.0 - scale_0_100(comp4 as f64, 3.0, 12.0);
        let c8_s = 100.0 - scale_0_100(comp8 as f64, 1.0, 8.0);
        0.50 * dot_s + 0.30 * c4_s + 0.20 * c8_s
    };

    let geom_s = if walls == 0 {
        50.0
    } else {
        let line_s = 100.0 * ortho_line_ratio;
        let compact_s = 100.0 - scale_0_100(perimeter_per_wall, 2.4, 4.0);
        0.55 * line_s + 0.45 * compact_s
    };

    let sym_s = match wall_symmetries {
        8 => 100.0,
        4 => 90.0,
        2 => 68.0,
        _ => 15.0,
    };

    let autocorr_s = scale_0_100(ortho_autocorr, -0.22, 0.45);
    // Breathable wall density: sweet spot ~6 to 12 walls (9%–19% on 8x8); penalize overcrowded 16+ wall boards.
    let density_s = trapezoid_0_100(p_wall, 0.04, 0.09, 0.19, 0.30);
    let order_s = 0.55 * autocorr_s + 0.45 * density_s;

    let score = 0.35 * cohesion_s + 0.30 * geom_s + 0.15 * sym_s + 0.20 * order_s;

    SimplicityMetrics {
        score,
        walls,
        playable,
        components_4: comp4,
        components_8: comp8,
        isolated_dot_ratio,
        ortho_line_ratio,
        perimeter_per_wall,
        wall_symmetries,
        ortho_autocorr,
    }
}

fn wall_components(board: &Board, dirs: &[(i32, i32)]) -> usize {
    let (w, h) = (board.w, board.h);
    let n = w * h;
    let mut seen = vec![false; n];
    let mut comps = 0;
    for i in 0..n {
        if board.cells[i] != Cell::Blocked || seen[i] {
            continue;
        }
        comps += 1;
        let mut q = VecDeque::new();
        q.push_back(i);
        seen[i] = true;
        while let Some(u) = q.pop_front() {
            let (ux, uy) = ((u % w) as i32, (u / w) as i32);
            for &(dx, dy) in dirs {
                let (nx, ny) = (ux + dx, uy + dy);
                if nx >= 0 && ny >= 0 && nx < w as i32 && ny < h as i32 {
                    let v = ny as usize * w + nx as usize;
                    if board.cells[v] == Cell::Blocked && !seen[v] {
                        seen[v] = true;
                        q.push_back(v);
                    }
                }
            }
        }
    }
    comps
}

/* ------------------------------------------------------------------ structural topology */

struct Topology {
    mean_copy_degree: f64,
    dead_ends: usize,
    cramped_ratio: f64,
    wall_jump_bridge_ratio: f64,
    copy_vs_jump_gap: f64,
    start_distance: f64,
    multi_lane_score: f64,
}

fn eval_topology(board: &Board) -> Topology {
    let (w, h) = (board.w, board.h);
    let n = w * h;
    let playable: Vec<usize> = (0..n).filter(|&i| board.cells[i] != Cell::Blocked).collect();
    let p_cnt = playable.len().max(1);

    // Pairwise shortest distances on the copy-only graph and the copy+jump graph.
    let copy_dists: Vec<Vec<u16>> = (0..n).map(|s| bfs_dist(board, s, false)).collect();
    let move_dists: Vec<Vec<u16>> = (0..n).map(|s| bfs_dist(board, s, true)).collect();

    // Copy degree distribution, dead ends, and cramped bottleneck cells.
    let mut degs = Vec::with_capacity(p_cnt);
    let mut dead_ends = 0usize;
    let mut cramped_cells = 0usize;
    let mut total_jump_edges = 0usize;
    let mut wall_bridge_weight = 0.0f64;

    for &u in &playable {
        let (ux, uy) = ((u % w) as i32, (u / w) as i32);
        let deg = AROUND
            .iter()
            .filter(|&&(dx, dy)| in_playable(board, ux + dx, uy + dy))
            .count();
        let ortho_deg = ORTHO
            .iter()
            .filter(|&&(dx, dy)| in_playable(board, ux + dx, uy + dy))
            .count();
        degs.push(deg as f64);
        // Dead end / cul-de-sac: <= 2 copy exits or <= 1 orthogonal exit (excluding natural outer corners if open).
        if deg <= 2 || ortho_deg <= 1 {
            dead_ends += 1;
        }
        if deg <= 3 {
            cramped_cells += 1;
        }

        for &(dx, dy) in &JUMPS {
            let (vx, vy) = (ux + dx, uy + dy);
            if !in_playable(board, vx, vy) {
                continue;
            }
            let v = vy as usize * w + vx as usize;
            total_jump_edges += 1;
            let mid = (uy + dy / 2) as usize * w + (ux + dx / 2) as usize;
            if board.cells[mid] == Cell::Blocked {
                wall_bridge_weight += if copy_dists[u][v] >= 3 { 1.0 } else { 0.6 };
            }
        }
    }

    let mean_copy_degree = degs.iter().sum::<f64>() / p_cnt as f64;
    let wall_jump_bridge_ratio =
        if total_jump_edges > 0 { wall_bridge_weight / total_jump_edges as f64 } else { 0.0 };

    // Average gap between copy-only distance and copy+jump distance.
    let mut gap_sum = 0.0;
    let mut pair_cnt = 0usize;
    for &u in &playable {
        for &v in &playable {
            if v <= u || move_dists[u][v] == u16::MAX {
                continue;
            }
            let cd = if copy_dists[u][v] == u16::MAX {
                (move_dists[u][v] as f64) + 5.0
            } else {
                copy_dists[u][v] as f64
            };
            gap_sum += (cd - move_dists[u][v] as f64).max(0.0);
            pair_cnt += 1;
        }
    }
    let copy_vs_jump_gap = if pair_cnt > 0 { gap_sum / pair_cnt as f64 } else { 0.0 };
    let cramped_ratio = cramped_cells as f64 / p_cnt as f64;

    // Distance and multi-lane connectivity between starting Green and Red positions.
    let g_pos = board.cells.iter().position(|&c| c == Cell::Piece(Player::Green));
    let r_pos = board.cells.iter().position(|&c| c == Cell::Piece(Player::Red));
    let (start_distance, multi_lane_score) = match (g_pos, r_pos) {
        (Some(gs), Some(rs)) => {
            let base = move_dists[gs][rs];
            let sd = if base < u16::MAX { base as f64 } else { 8.0 };
            let ml = measure_multi_lane(board, gs, rs, &move_dists[gs], &move_dists[rs]);
            (sd, ml)
        }
        _ => (0.0, 0.5),
    };

    Topology {
        mean_copy_degree,
        dead_ends,
        cramped_ratio,
        wall_jump_bridge_ratio,
        copy_vs_jump_gap,
        start_distance,
        multi_lane_score,
    }
}

/// Measures how widely spread the near-shortest paths between Green and Red are:
/// wide multi-front corridors score high; a single narrow funnel scores low.
fn measure_multi_lane(board: &Board, gs: usize, rs: usize, dg: &[u16], dr: &[u16]) -> f64 {
    let base = dg[rs];
    if base == u16::MAX || base < 2 {
        return 0.0;
    }
    let (w, h) = (board.w, board.h);
    let mid_cells: Vec<(f64, f64)> = (0..(w * h))
        .filter(|&i| {
            i != gs
                && i != rs
                && board.cells[i] != Cell::Blocked
                && dg[i] < u16::MAX
                && dr[i] < u16::MAX
                && dg[i] + dr[i] <= base + 1
                && (dg[i] as i32 - dr[i] as i32).abs() <= 1
        })
        .map(|i| ((i % w) as f64, (i / w) as f64))
        .collect();
    if mid_cells.len() < 2 {
        return 0.1;
    }
    let mut max_span = 0.0f64;
    for i in 0..mid_cells.len() {
        for j in (i + 1)..mid_cells.len() {
            let d = (mid_cells[i].0 - mid_cells[j].0).hypot(mid_cells[i].1 - mid_cells[j].1);
            max_span = max_span.max(d);
        }
    }
    let diag = (w as f64).hypot(h as f64);
    let span_ratio = (max_span / (diag * 0.72)).min(1.0);
    let width_ratio = (mid_cells.len() as f64 / 8.0).min(1.0);
    0.65 * span_ratio + 0.35 * width_ratio
}

fn in_playable(board: &Board, x: i32, y: i32) -> bool {
    x >= 0
        && y >= 0
        && x < board.w as i32
        && y < board.h as i32
        && board.cells[y as usize * board.w + x as usize] != Cell::Blocked
}

fn bfs_dist(board: &Board, start: usize, allow_jumps: bool) -> Vec<u16> {
    let (w, h) = (board.w, board.h);
    let n = w * h;
    let mut dist = vec![u16::MAX; n];
    if board.cells[start] == Cell::Blocked {
        return dist;
    }
    let mut q = VecDeque::new();
    dist[start] = 0;
    q.push_back(start);
    while let Some(u) = q.pop_front() {
        let d = dist[u] + 1;
        let (ux, uy) = ((u % w) as i32, (u / w) as i32);
        for &(dx, dy) in &AROUND {
            if in_playable(board, ux + dx, uy + dy) {
                let v = (uy + dy) as usize * w + (ux + dx) as usize;
                if dist[v] == u16::MAX {
                    dist[v] = d;
                    q.push_back(v);
                }
            }
        }
        if allow_jumps {
            for &(dx, dy) in &JUMPS {
                if in_playable(board, ux + dx, uy + dy) {
                    let v = (uy + dy) as usize * w + (ux + dx) as usize;
                    if dist[v] == u16::MAX {
                        dist[v] = d;
                        q.push_back(v);
                    }
                }
            }
        }
    }
    dist
}

/* ------------------------------------------------------------------ opening analysis */

struct OpeningSummary {
    viable_count: f64,
    spatial_spread: f64,
    start_eval_cells: f64,
}

fn eval_openings(board: &Board) -> OpeningSummary {
    let mut search = Search::new(None);
    let g_moves = search.score_moves(board, Player::Green, OPEN_DEPTH);
    if g_moves.is_empty() {
        return OpeningSummary { viable_count: 0.0, spatial_spread: 0.0, start_eval_cells: 0.0 };
    }
    let start_eval_cells = g_moves[0].1 as f64 / ai::SCALE as f64;
    let (c0, s0) = opening_breadth(board, &g_moves);

    // Also inspect Red's reply to Green's best move and Green's 2nd move for fuller opening picture.
    let mut b1 = board.clone();
    b1.apply(Player::Green, g_moves[0].0);
    let r_moves = search.score_moves(&b1, Player::Red, OPEN_DEPTH - 1);
    let (c1, s1) = opening_breadth(&b1, &r_moves);

    let (c2, s2) = if let Some(&(rm, _)) = r_moves.first() {
        let mut b2 = b1.clone();
        b2.apply(Player::Red, rm);
        let g2 = search.score_moves(&b2, Player::Green, OPEN_DEPTH - 1);
        opening_breadth(&b2, &g2)
    } else {
        (0.0, 0.0)
    };

    OpeningSummary {
        viable_count: 0.45 * c0 + 0.35 * c1 + 0.20 * c2,
        spatial_spread: 0.45 * s0 + 0.35 * s1 + 0.20 * s2,
        start_eval_cells,
    }
}

fn opening_breadth(board: &Board, scored: &[(Move, i32)]) -> (f64, f64) {
    let Some(&(_, best)) = scored.first() else { return (0.0, 0.0) };
    let viable: Vec<Move> = scored.iter().filter(|&&(_, s)| best - s <= 85).map(|&(m, _)| m).collect();
    let count = viable.len() as f64;
    if viable.len() < 2 {
        return (count, 0.0);
    }
    let w = board.w;
    let mut sum = 0.0;
    let mut pairs = 0usize;
    for i in 0..viable.len() {
        let (x1, y1) = ((viable[i].to % w) as f64, (viable[i].to / w) as f64);
        for mv in &viable[(i + 1)..] {
            let (x2, y2) = ((mv.to % w) as f64, (mv.to / w) as f64);
            sum += (x1 - x2).hypot(y1 - y2);
            pairs += 1;
        }
    }
    let spread = (sum / pairs as f64) / (w as f64);
    (count, spread)
}

/* ------------------------------------------------------------------ self-play simulations */

struct SimSummary {
    avg_plies: f64,
    non_greedy_rate: f64,
    alive_fraction: f64,
    contested_ratio: f64,
    late_swings: f64,
    final_closeness: f64,
    contact_conversions: f64,
    late_conversions: f64,
    wall_jump_rate: f64,
    jump_rate: f64,
    trajectory_divergence: f64,
    green_win_rate: f64,
}

fn run_simulations(board: &Board, seed: u64) -> SimSummary {
    let mut rng = StdRng::seed_from_u64(seed);
    let playable = board.cells.iter().filter(|&&c| c != Cell::Blocked).count().max(1) as f64;

    let mut total_plies = 0usize;
    let mut non_greedy_hits = 0usize;
    let mut non_greedy_checks = 0usize;
    let mut alive_frac_sum = 0.0;
    let mut contested_plies = 0usize;
    let mut eval_plies = 0usize;
    let mut late_swings_total = 0usize;
    let mut closeness_sum = 0.0;
    let mut contact_moves = 0usize;
    let mut contact_convs = 0usize;
    let mut late_moves = 0usize;
    let mut late_convs = 0usize;
    let mut total_moves = 0usize;
    let mut jump_moves = 0usize;
    let mut wall_jump_moves = 0usize;
    let mut green_score_pts = 0.0;
    let mut mid_snapshots: Vec<Vec<Cell>> = Vec::with_capacity(SIM_GAMES);

    for game_idx in 0..SIM_GAMES {
        let mut g = Game::new(board.clone());
        let mut board_hashes = vec![ai::board_hash(&g.board)];
        let mut side_hashes = [
            vec![ai::side_hash(&g.board, Player::Green)],
            vec![ai::side_hash(&g.board, Player::Red)],
        ];
        let mut streak = [0u8; 2];
        let mut search = Search::new(None);
        let mut in_contact = false;
        let mut mid_saved = false;
        let mut resigned_winner: Option<Player> = None;
        let mut ply_log: Vec<(f64, usize, usize)> = Vec::with_capacity(96); // (wp_green, convs, player_idx)

        while !g.over && g.ply < MAX_SIM_PLY {
            let p = g.turn;
            let hist = History {
                boards: board_hashes.clone(),
                my_sides: side_hashes[p.idx()].clone(),
            };

            // Resignation check for clean termination once genuinely hopeless.
            if g.ply >= 18 {
                if let Some(out) = ai::decide(&g.board, p, Difficulty::Medium, g.ply, &hist, streak[p.idx()]) {
                    streak[p.idx()] = out.hopeless_streak;
                    if out.decision == Decision::Resign {
                        resigned_winner = Some(p.other());
                        break;
                    }
                }
            }

            search.set_history(&hist);
            let mut scored = search.score_moves(&g.board, p, SIM_DEPTH);
            if scored.is_empty() {
                break;
            }
            // Apply anti-repetition penalty so simulated games never stall.
            for (m, s) in &mut scored {
                let mut tmp = g.board.clone();
                let conv = tmp.apply(p, *m);
                let rep = hist.boards.contains(&ai::board_hash(&tmp))
                    || (g.board.is_jump(*m) && conv.is_empty() && hist.my_sides.contains(&ai::side_hash(&tmp, p)));
                if rep && s.abs() < WIN {
                    *s -= 300;
                }
            }
            scored.sort_by_key(|&(_, s)| -s);

            let (best_m, best_s) = scored[0];

            // Check non-greedy tactical richness on non-trivial positions.
            if scored.len() >= 3 && g.board.empties() >= 8 {
                let max_gain = scored.iter().map(|&(m, _)| immediate_gain(&g.board, p, m)).max().unwrap_or(0);
                let best_gain = immediate_gain(&g.board, p, best_m);
                non_greedy_checks += 1;
                if best_gain < max_gain {
                    non_greedy_hits += 1;
                }
            }

            // Pick a strong but opening-diverse move so the 16 games explore different plans.
            let chosen = if game_idx == 0 {
                best_m
            } else if g.ply < 6 {
                let band = 110;
                let pool: Vec<(Move, i32)> =
                    scored.iter().copied().filter(|&(_, s)| best_s - s <= band).take(5).collect();
                pick_weighted(&pool, 55.0, &mut rng)
            } else {
                let pool: Vec<(Move, i32)> =
                    scored.iter().copied().filter(|&(_, s)| best_s - s <= 35).take(3).collect();
                pick_weighted(&pool, 25.0, &mut rng)
            };

            // Record per-ply win-prob and conversions so we can measure late-game vitality
            // and when the game actually became decided (anti-"filling" measurement).
            let green_eval = if p == Player::Green { best_s } else { -best_s };
            let wp = ai::win_prob(green_eval);
            eval_plies += 1;
            if (0.15..=0.85).contains(&wp) {
                contested_plies += 1;
            }

            let is_jump = g.board.is_jump(chosen);
            let is_wall_jump = is_jump && jumps_over_wall(&g.board, chosen);
            let convs = g.board.captures(p, chosen.to).count();
            ply_log.push((wp, convs, p.idx()));
            if convs > 0 {
                in_contact = true;
            }
            total_moves += 1;
            if is_jump {
                jump_moves += 1;
            }
            if is_wall_jump {
                wall_jump_moves += 1;
            }
            if in_contact {
                contact_moves += 1;
                contact_convs += convs;
            }

            g.play(p, chosen).unwrap();
            board_hashes.push(ai::board_hash(&g.board));
            side_hashes[p.idx()].push(ai::side_hash(&g.board, p));

            if g.ply == 16 && !mid_saved {
                mid_snapshots.push(g.board.cells.clone());
                mid_saved = true;
            }
        }

        // Analyze full-game vitality: how late the game remained undecided, and how active
        // the second half was (conversions + same-parity swings).
        let n_log = ply_log.len();
        if n_log > 0 {
            let last_alive = ply_log
                .iter()
                .enumerate()
                .rev()
                .find(|&(_, &(wp, _, _))| (0.12..=0.88).contains(&wp))
                .map_or(0, |(idx, _)| idx + 1);
            alive_frac_sum += last_alive as f64 / n_log as f64;

            let half = n_log / 2;
            let mut prev_wp = [None::<f64>; 2];
            for (idx, &(wp, convs, pidx)) in ply_log.iter().enumerate() {
                if idx >= half {
                    late_moves += 1;
                    late_convs += convs;
                    if let Some(prev) = prev_wp[pidx] {
                        if (prev - 0.5) * (wp - 0.5) < 0.0 || (wp - prev).abs() >= 0.14 {
                            late_swings_total += 1;
                        }
                    }
                }
                prev_wp[pidx] = Some(wp);
            }
        }

        if !mid_saved {
            mid_snapshots.push(g.board.cells.clone());
        }

        total_plies += g.ply as usize;
        let (gs, rs) = (g.score(Player::Green) as f64, g.score(Player::Red) as f64);
        closeness_sum += (1.0 - (gs - rs).abs() / playable).clamp(0.0, 1.0);
        let winner = resigned_winner.or_else(|| g.leader());
        green_score_pts += match winner {
            Some(Player::Green) => 1.0,
            Some(Player::Red) => 0.0,
            None => 0.5,
        };
    }

    // Average pairwise midgame trajectory divergence across simulated games.
    let mut div_sum = 0.0;
    let mut div_pairs = 0usize;
    for i in 0..mid_snapshots.len() {
        for j in (i + 1)..mid_snapshots.len() {
            let diff = mid_snapshots[i]
                .iter()
                .zip(mid_snapshots[j].iter())
                .filter(|&(a, b)| a != b)
                .count();
            div_sum += diff as f64 / playable;
            div_pairs += 1;
        }
    }

    SimSummary {
        avg_plies: total_plies as f64 / SIM_GAMES as f64,
        non_greedy_rate: if non_greedy_checks > 0 {
            non_greedy_hits as f64 / non_greedy_checks as f64
        } else {
            0.0
        },
        alive_fraction: alive_frac_sum / SIM_GAMES as f64,
        contested_ratio: if eval_plies > 0 { contested_plies as f64 / eval_plies as f64 } else { 0.0 },
        late_swings: late_swings_total as f64 / SIM_GAMES as f64,
        final_closeness: closeness_sum / SIM_GAMES as f64,
        contact_conversions: if contact_moves > 0 { contact_convs as f64 / contact_moves as f64 } else { 0.0 },
        late_conversions: if late_moves > 0 { late_convs as f64 / late_moves as f64 } else { 0.0 },
        wall_jump_rate: if total_moves > 0 { wall_jump_moves as f64 / total_moves as f64 } else { 0.0 },
        jump_rate: if total_moves > 0 { jump_moves as f64 / total_moves as f64 } else { 0.0 },
        trajectory_divergence: if div_pairs > 0 { div_sum / div_pairs as f64 } else { 0.0 },
        green_win_rate: green_score_pts / SIM_GAMES as f64,
    }
}

fn immediate_gain(board: &Board, p: Player, m: Move) -> i32 {
    let caps = board.captures(p, m.to).count() as i32;
    2 * caps + i32::from(!board.is_jump(m))
}

fn jumps_over_wall(board: &Board, m: Move) -> bool {
    let w = board.w;
    let mx = (m.from % w + m.to % w) / 2;
    let my = (m.from / w + m.to / w) / 2;
    board.cells[my * w + mx] == Cell::Blocked
}

fn pick_weighted(pool: &[(Move, i32)], temp: f64, rng: &mut StdRng) -> Move {
    if pool.len() == 1 {
        return pool[0].0;
    }
    let best = pool[0].1 as f64;
    let weights: Vec<f64> = pool.iter().map(|&(_, s)| ((s as f64 - best) / temp).exp()).collect();
    let total: f64 = weights.iter().sum();
    let mut r = rng.random::<f64>() * total;
    for (&(m, _), &w) in pool.iter().zip(weights.iter()) {
        r -= w;
        if r <= 0.0 {
            return m;
        }
    }
    pool[0].0
}

/* ------------------------------------------------------------------ distinctness */

/// Maps cell index `i` under dihedral transform `t` in `0..8` (4 rotations × 2 reflections).
fn dihedral_idx(w: usize, h: usize, i: usize, t: usize) -> usize {
    let x = i % w;
    let y = i / w;
    if w != h {
        // Non-square boards only support 4 symmetries (identity, H-flip, V-flip, 180°).
        let (nx, ny) = match t % 4 {
            0 => (x, y),
            1 => (w - 1 - x, y),
            2 => (x, h - 1 - y),
            _ => (w - 1 - x, h - 1 - y),
        };
        return ny * w + nx;
    }
    let (rx, ry) = match t % 4 {
        0 => (x, y),
        1 => (w - 1 - y, x),
        2 => (w - 1 - x, w - 1 - y),
        _ => (y, w - 1 - x),
    };
    let (fx, fy) = if t >= 4 { (w - 1 - rx, ry) } else { (rx, ry) };
    fy * w + fx
}

/// Minimum layout distance between two boards over all 8 dihedral symmetries
/// (and both color assignments). Combines wall Jaccard distance, soft 1-cell
/// chamfer wall distance, and starting-position alignment.
pub fn dihedral_layout_distance(a: &Board, b: &Board) -> f64 {
    if a.w != b.w || a.h != b.h {
        return 1.0;
    }
    let (w, h) = (a.w, a.h);
    let n = w * h;
    let diag = (w as f64).hypot(h as f64);

    let a_walls: Vec<bool> = a.cells.iter().map(|&c| c == Cell::Blocked).collect();
    let a_starts = start_coords(a);
    let mut best = 1.0f64;

    for t in 0..8 {
        let mut inter = 0usize;
        let mut union = 0usize;
        let mut b_walls = vec![false; n];
        for i in 0..n {
            let bw = b.cells[dihedral_idx(w, h, i, t)] == Cell::Blocked;
            b_walls[i] = bw;
            if a_walls[i] && bw {
                inter += 1;
            }
            if a_walls[i] || bw {
                union += 1;
            }
        }
        let jaccard_dist = if union > 0 { 1.0 - inter as f64 / union as f64 } else { 0.0 };
        let chamfer_dist = (chamfer_walls(w, &a_walls, &b_walls) / 2.5).min(1.0);
        let wall_dist = 0.65 * jaccard_dist + 0.35 * chamfer_dist;

        // Starting positions of B under transform `t` (match either color assignment).
        let b_starts = transformed_starts(b, t);
        let start_dist = match (a_starts, b_starts) {
            (Some((ag, ar)), Some((bg, br))) => {
                let d_same = (dist2d(ag, bg) + dist2d(ar, br)) / (2.0 * diag);
                let d_swap = (dist2d(ag, br) + dist2d(ar, bg)) / (2.0 * diag);
                d_same.min(d_swap)
            }
            _ => 0.0,
        };

        // Weigh wall layout heavily (0.80) so identical wall patterns with swapped corners
        // (like Scatter vs Drift) are clearly flagged as near-clones.
        let combined = 0.80 * wall_dist + 0.20 * start_dist;
        best = best.min(combined);
    }
    best
}

fn chamfer_walls(w: usize, a: &[bool], b: &[bool]) -> f64 {
    let pts = |mask: &[bool]| -> Vec<(f64, f64)> {
        mask.iter()
            .enumerate()
            .filter_map(|(i, &on)| on.then_some(((i % w) as f64, (i / w) as f64)))
            .collect()
    };
    let pa = pts(a);
    let pb = pts(b);
    if pa.is_empty() && pb.is_empty() {
        return 0.0;
    }
    if pa.is_empty() || pb.is_empty() {
        return 2.5;
    }
    let avg_min = |from: &[(f64, f64)], to: &[(f64, f64)]| -> f64 {
        from.iter()
            .map(|&p| to.iter().map(|&q| dist2d(p, q)).fold(f64::INFINITY, f64::min))
            .sum::<f64>()
            / from.len() as f64
    };
    0.5 * (avg_min(&pa, &pb) + avg_min(&pb, &pa))
}

fn start_coords(board: &Board) -> Option<((f64, f64), (f64, f64))> {
    let g = board.cells.iter().position(|&c| c == Cell::Piece(Player::Green))?;
    let r = board.cells.iter().position(|&c| c == Cell::Piece(Player::Red))?;
    let w = board.w;
    Some((((g % w) as f64, (g / w) as f64), ((r % w) as f64, (r / w) as f64)))
}

fn transformed_starts(board: &Board, t: usize) -> Option<((f64, f64), (f64, f64))> {
    let (w, h) = (board.w, board.h);
    let mut g = None;
    let mut r = None;
    for i in 0..(w * h) {
        match board.cells[dihedral_idx(w, h, i, t)] {
            Cell::Piece(Player::Green) => g = Some(((i % w) as f64, (i / w) as f64)),
            Cell::Piece(Player::Red) => r = Some(((i % w) as f64, (i / w) as f64)),
            _ => {}
        }
    }
    Some((g?, r?))
}

fn dist2d(a: (f64, f64), b: (f64, f64)) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

fn fingerprint_distance(a: &[f64; 10], b: &[f64; 10], scales: &[f64; 10]) -> f64 {
    let sq: f64 = a
        .iter()
        .zip(b.iter())
        .zip(scales.iter())
        .map(|((&x, &y), &s)| ((x - y) / s).powi(2))
        .sum();
    let rms = (sq / a.len() as f64).sqrt();
    (rms / 2.2).min(1.0)
}

/* ------------------------------------------------------------------ scaling helpers */

fn scale_0_100(val: f64, lo: f64, hi: f64) -> f64 {
    (((val - lo) / (hi - lo)).clamp(0.0, 1.0)) * 100.0
}

fn trapezoid_0_100(val: f64, a: f64, b: f64, c: f64, d: f64) -> f64 {
    if val <= a || val >= d {
        0.0
    } else if val < b {
        100.0 * (val - a) / (b - a)
    } else if val <= c {
        100.0
    } else {
        100.0 * (d - val) / (d - c)
    }
}

/* ------------------------------------------------------------------ CLI */

pub fn run_cli(args: &[String]) {
    let as_json = args.iter().any(|a| a == "--json");
    let only_custom = args.iter().any(|a| a == "--only");
    let mut boards: Vec<(String, Board)> = if only_custom {
        Vec::new()
    } else {
        maps::MAPS.iter().map(|m| (m.name.to_string(), Board::parse(m.rows))).collect()
    };

    for arg in args.iter().filter(|a| !a.starts_with('-')) {
        match load_custom_maps(arg) {
            Ok(extra) => boards.extend(extra),
            Err(e) => {
                eprintln!("Cannot load {arg}: {e}");
                std::process::exit(1);
            }
        }
    }

    let refs: Vec<(&str, Board)> = boards.iter().map(|(n, b)| (n.as_str(), b.clone())).collect();
    let report = evaluate_pool(&refs);

    if as_json {
        println!("{}", serde_json::to_string_pretty(&report).expect("serialize report"));
        return;
    }

    print_report(&report);
}

/// Loads one or more ASCII maps from a text file (separated by blank lines; optional `# Name` header).
fn load_custom_maps(path: &str) -> Result<Vec<(String, Board)>, String> {
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    let mut name = path.to_string();
    let mut rows: Vec<&str> = Vec::new();
    let flush = |out: &mut Vec<(String, Board)>, name: &str, rows: &mut Vec<&str>| -> Result<(), String> {
        if rows.is_empty() {
            return Ok(());
        }
        let w = rows[0].len();
        if !rows.iter().all(|r| r.len() == w) {
            return Err(format!("ragged rows in {name}"));
        }
        let b = Board::decode(w, rows.len(), &rows.concat()).ok_or_else(|| format!("invalid cells in {name}"))?;
        out.push((name.to_string(), b));
        rows.clear();
        Ok(())
    };
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            flush(&mut out, &name, &mut rows)?;
        } else if let Some(rest) = line.strip_prefix("//") {
            name = rest.trim().to_string();
        } else {
            rows.push(line);
        }
    }
    flush(&mut out, &name, &mut rows)?;
    if out.is_empty() {
        return Err("no boards found".into());
    }
    Ok(out)
}

fn print_report(report: &PoolReport) {
    let mut order: Vec<usize> = (0..report.boards.len()).collect();
    order.sort_by(|&a, &b| report.boards[b].overall.total_cmp(&report.boards[a].overall));

    println!("=== BACTERIA BOARD EVALUATION ({SIM_GAMES} simulated games/board, depth {SIM_DEPTH}/{OPEN_DEPTH}) ===\n");
    println!(
        "{:<10} {:>7} | {:>11} {:>9} {:>10} {:>10} | {:>5}  Closest Neighbor",
        "Board", "OVERALL", "Interaction", "Diversity", "Simplicity", "Distinct", "Plies"
    );
    println!("{}", "-".repeat(92));
    for &i in &order {
        let b = &report.boards[i];
        println!(
            "{:<10} {:>7.1} | {:>11.1} {:>9.1} {:>10.1} {:>10.1} | {:>5.1}  {} (dist {:.2}, layout {:.2})",
            b.name,
            b.overall,
            b.interaction.score,
            b.diversity.score,
            b.simplicity.score,
            b.distinctness.score,
            b.avg_plies,
            b.distinctness.nearest_neighbor,
            b.distinctness.nearest_dist,
            b.distinctness.nearest_layout_dist,
        );
    }

    println!("\n--- Detailed Sub-Metrics ---");
    for &i in &order {
        let b = &report.boards[i];
        let it = &b.interaction;
        let dv = &b.diversity;
        let sm = &b.simplicity;
        println!("\n[{}] Overall {:.1}", b.name, b.overall);
        println!(
            "  Interaction ({:4.1}): alive {:4.0}% | late-conv {:.2} | late-swings {:.1}/g | conv/mv {:.2} | non-greedy {:4.0}% | closeness {:4.0}% | wall-jumps {:4.1}% | dead-ends {} | avg-deg {:.2}",
            it.score,
            it.alive_fraction * 100.0,
            it.late_conversions,
            it.late_swings,
            it.contact_conversions,
            it.non_greedy_rate * 100.0,
            it.final_closeness * 100.0,
            it.wall_jump_rate * 100.0,
            it.dead_ends,
            it.mean_copy_degree,
        );
        println!(
            "  Diversity   ({:4.1}): viable openings {:.1} (spread {:.2}) | midgame div {:4.0}% | jumps {:4.1}% | start eval {:+.2} | Green win {:4.0}% | multi-lane {:.2}",
            dv.score,
            dv.viable_openings,
            dv.opening_spread,
            dv.trajectory_divergence * 100.0,
            dv.jump_rate * 100.0,
            dv.start_eval_cells,
            dv.green_win_rate * 100.0,
            dv.multi_lane_score,
        );
        println!(
            "  Simplicity  ({:4.1}): {} walls ({} playable) | comp4={} comp8={} | isolated dots {:4.0}% | ortho lines {:4.0}% | perim/wall {:.2} | sym={} | autocorr {:+.2}",
            sm.score,
            sm.walls,
            sm.playable,
            sm.components_4,
            sm.components_8,
            sm.isolated_dot_ratio * 100.0,
            sm.ortho_line_ratio * 100.0,
            sm.perimeter_per_wall,
            sm.wall_symmetries,
            sm.ortho_autocorr,
        );
        println!(
            "  Distinct    ({:4.1}): nearest={} (combined {:.2}, layout {:.2}, play {:.2}) | mean dist {:.2}",
            b.distinctness.score,
            b.distinctness.nearest_neighbor,
            b.distinctness.nearest_dist,
            b.distinctness.nearest_layout_dist,
            b.distinctness.nearest_play_dist,
            b.distinctness.mean_dist,
        );
    }

    println!("\n--- Pairwise Distance Matrix (combined / layout-only) ---");
    print!("{:<10}", "");
    for b in &report.boards {
        print!(" {:>11}", b.name);
    }
    println!();
    for (i, b) in report.boards.iter().enumerate() {
        print!("{:<10}", b.name);
        for j in 0..report.boards.len() {
            if i == j {
                print!(" {:>11}", "-");
            } else {
                print!(" {:>5.2}/{:<5.2}", report.pairwise_distances[i][j], report.pairwise_layout_distances[i][j]);
            }
        }
        println!();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dihedral_distance_is_zero_for_rotations_and_flips() {
        let a = Board::parse(maps::MAPS[1].rows); // Cascade
        let b = Board::parse(&[
            "G.......",
            ".....#..",
            ".##..#..",
            ".....#..",
            "..#.....",
            "..#..##.",
            "..#.....",
            ".......R",
        ]); // Cascade rotated 180° with colours swapped
        assert!(dihedral_layout_distance(&a, &b) < 1e-9);
    }

    #[test]
    fn near_clone_walls_have_small_layout_distance() {
        let base = Board::parse(&[
            "R.......",
            ".#...#..",
            "..#.#...",
            ".....#..",
            "..#.....",
            "...#.#..",
            "..#...#.",
            ".......G",
        ]);
        let clone = Board::parse(&[
            ".......R",
            ".#...#..",
            "..#.#.#.",
            ".....#..",
            "..#.....",
            ".#.#.#..",
            "..#...#.",
            "G.......",
        ]);
        let distinct = Board::parse(maps::MAPS[4].rows); // Atoll
        let d_clone = dihedral_layout_distance(&base, &clone);
        let d_other = dihedral_layout_distance(&base, &distinct);
        assert!(d_clone < 0.25, "Near-clone layouts should be very close, got {d_clone}");
        assert!(d_clone < d_other, "Near-clone ({d_clone}) should be closer than Atoll ({d_other})");
    }

    #[test]
    fn clean_shapes_beat_confetti_on_visual_simplicity() {
        let atoll = eval_simplicity(&Board::parse(maps::MAPS[4].rows));
        let compass = eval_simplicity(&Board::parse(maps::MAPS[5].rows));
        let confetti = eval_simplicity(&Board::parse(&[
            "R.......",
            ".#...#..",
            "..#.#...",
            ".....#..",
            "..#.....",
            "...#.#..",
            "..#...#.",
            ".......G",
        ]));
        assert!(
            atoll.score > confetti.score + 40.0,
            "Atoll ({}) should beat confetti ({})",
            atoll.score,
            confetti.score
        );
        assert!(
            compass.score > confetti.score + 40.0,
            "Compass ({}) should beat confetti ({})",
            compass.score,
            confetti.score
        );
    }

    #[test]
    fn built_in_maps_have_zero_dead_ends_and_open_flow() {
        for map in maps::MAPS {
            let topo = eval_topology(&Board::parse(map.rows));
            assert_eq!(topo.dead_ends, 0, "{} should have zero dead ends", map.name);
            assert!(topo.mean_copy_degree >= 5.0, "{} should stay open (got {:.2})", map.name, topo.mean_copy_degree);
        }
    }
}
