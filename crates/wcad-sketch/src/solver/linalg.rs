//! Symmetric envelope (skyline) matrices and their Cholesky factorization.
//!
//! Sketch normal matrices (`J W⁻¹ Jᵀ`, `Jᵀ J`, `J Jᵀ`) are very sparse and, with equations and
//! parameters numbered in creation order, have a narrow profile. Row `i` stores the lower-triangle
//! entries from column `first[i]` to the diagonal contiguously; the Cholesky factor has the same
//! envelope (fill-in stays inside it), so the cost is `O(sum profile * bandwidth)` instead of
//! `O(n³)` and the memory is the profile instead of `n²`.

/// Lower triangle of a symmetric matrix in skyline storage.
#[derive(Clone, Debug, Default)]
pub(crate) struct Skyline {
    pub(crate) n: usize,
    /// `first[i]` = column of the first stored entry of row `i` (`<= i`).
    pub(crate) first: Vec<usize>,
    /// Row `i` occupies `a[start[i]..start[i + 1]]`, columns `first[i]..=i`.
    pub(crate) start: Vec<usize>,
    pub(crate) a: Vec<f64>,
}

impl Skyline {
    /// Set the structure (`first` per row, each `<= row`) and zero all entries.
    pub(crate) fn set_structure(&mut self, first: &[usize]) {
        self.n = first.len();
        self.first.clear();
        self.first
            .extend(first.iter().enumerate().map(|(i, &f)| f.min(i)));
        self.start.clear();
        self.start.reserve(self.n + 1);
        let mut off = 0;
        for i in 0..self.n {
            self.start.push(off);
            off += i + 1 - self.first[i];
        }
        self.start.push(off);
        self.a.clear();
        self.a.resize(off, 0.0);
    }

    /// Same structure as `o`, copied values.
    pub(crate) fn copy_from(&mut self, o: &Skyline) {
        self.n = o.n;
        self.first.clear();
        self.first.extend_from_slice(&o.first);
        self.start.clear();
        self.start.extend_from_slice(&o.start);
        self.a.clear();
        self.a.extend_from_slice(&o.a);
    }

    #[inline]
    pub(crate) fn idx(&self, i: usize, j: usize) -> usize {
        debug_assert!(j <= i && j >= self.first[i]);
        self.start[i] + (j - self.first[i])
    }

    /// `A[i][j] += v` (either triangle; the entry must lie inside the envelope).
    #[inline]
    pub(crate) fn add(&mut self, i: usize, j: usize, v: f64) {
        let (i, j) = if i >= j { (i, j) } else { (j, i) };
        let k = self.idx(i, j);
        self.a[k] += v;
    }

    #[inline]
    pub(crate) fn diag(&self, i: usize) -> f64 {
        self.a[self.start[i + 1] - 1]
    }

    #[inline]
    pub(crate) fn add_diag(&mut self, i: usize, v: f64) {
        let k = self.start[i + 1] - 1;
        self.a[k] += v;
    }

    pub(crate) fn max_diag(&self) -> f64 {
        (0..self.n).map(|i| self.diag(i)).fold(0.0, f64::max)
    }

    /// Stored lower-triangle entries.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn profile(&self) -> usize {
        self.a.len()
    }

    /// Dot product of rows `i` and `j` of the (partial) factor over columns `k0..j`.
    #[inline]
    pub(crate) fn row_dot(&self, i: usize, j: usize, k0: usize) -> f64 {
        let (fi, fj) = (self.first[i], self.first[j]);
        let ri = &self.a[self.start[i] + (k0 - fi)..self.start[i] + (j - fi)];
        let rj = &self.a[self.start[j] + (k0 - fj)..self.start[j] + (j - fj)];
        ri.iter().zip(rj).map(|(x, y)| x * y).sum()
    }

    /// In-place Cholesky `A = L Lᵀ`. Returns `false` if not (numerically) positive definite.
    pub(crate) fn cholesky(&mut self) -> bool {
        for i in 0..self.n {
            let fi = self.first[i];
            for j in fi..=i {
                let k0 = fi.max(self.first[j]);
                let ij = self.idx(i, j);
                let s = self.a[ij] - self.row_dot(i, j, k0);
                if j < i {
                    self.a[ij] = s / self.diag(j);
                } else {
                    if !(s > 0.0) || !s.is_finite() {
                        return false;
                    }
                    self.a[ij] = s.sqrt();
                }
            }
        }
        true
    }

    /// Solve `(L Lᵀ) x = b` in place after [`Skyline::cholesky`].
    pub(crate) fn solve(&self, b: &mut [f64]) {
        let n = self.n;
        for i in 0..n {
            let (f, s0) = (self.first[i], self.start[i]);
            let row = &self.a[s0..self.start[i + 1]];
            let mut s = b[i];
            for (k, l) in (f..i).zip(row) {
                s -= l * b[k];
            }
            b[i] = s / row[i - f];
        }
        for i in (0..n).rev() {
            let (f, s0) = (self.first[i], self.start[i]);
            let row = &self.a[s0..self.start[i + 1]];
            b[i] /= row[i - f];
            let bi = b[i];
            for (k, l) in (f..i).zip(row) {
                b[k] -= l * bi;
            }
        }
    }
}

/// Reverse Cuthill-McKee ordering of a symmetric graph given as adjacency lists (`adj[v]` = the
/// neighbours of `v`). Returns `perm` with `perm[new] = old`. Keeps the envelope narrow when the
/// natural order is scattered (constraints added long after their geometry).
pub(crate) fn rcm(adj: &[Vec<u32>]) -> Vec<u32> {
    let n = adj.len();
    let mut order: Vec<u32> = Vec::with_capacity(n);
    let mut seen = vec![false; n];
    let mut by_degree: Vec<u32> = (0..n as u32).collect();
    by_degree.sort_by_key(|&v| adj[v as usize].len());
    let mut nbrs: Vec<u32> = Vec::new();
    for &root in &by_degree {
        if seen[root as usize] {
            continue;
        }
        seen[root as usize] = true;
        let mut head = order.len();
        order.push(root);
        while head < order.len() {
            let v = order[head] as usize;
            head += 1;
            nbrs.clear();
            nbrs.extend(adj[v].iter().copied().filter(|&u| !seen[u as usize]));
            nbrs.sort_by_key(|&u| adj[u as usize].len());
            nbrs.dedup();
            for &u in &nbrs {
                if !seen[u as usize] {
                    seen[u as usize] = true;
                    order.push(u);
                }
            }
        }
    }
    order.reverse();
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    fn banded(n: usize, first: &[usize]) -> Skyline {
        let mut m = Skyline::default();
        m.set_structure(first);
        for i in 0..n {
            m.add(i, i, 4.0 + i as f64 * 0.1);
            if i >= 1 && first[i] < i {
                m.add(i, i - 1, -1.0);
            }
            if i >= 3 && first[i] <= i - 3 {
                m.add(i, i - 3, 0.5);
            }
        }
        m
    }

    #[test]
    fn skyline_matches_dense() {
        let n = 30;
        let narrow: Vec<usize> = (0..n).map(|i: usize| i.saturating_sub(3)).collect();
        let dense = [0; 30];
        let m = banded(n, &narrow);
        let d = banded(n, &dense);
        assert!(m.profile() < d.profile());
        let b0: Vec<f64> = (0..n).map(|i| (i as f64).sin()).collect();
        let (mut x1, mut x2) = (b0.clone(), b0.clone());
        let mut f1 = m.clone();
        assert!(f1.cholesky());
        f1.solve(&mut x1);
        let mut f2 = d.clone();
        assert!(f2.cholesky());
        f2.solve(&mut x2);
        for i in 0..n {
            assert!((x1[i] - x2[i]).abs() < 1e-12);
            let mut ax = 0.0;
            for (j, xj) in x1.iter().enumerate() {
                let (r, c) = if i >= j { (i, j) } else { (j, i) };
                if c >= m.first[r] {
                    ax += m.a[m.idx(r, c)] * xj;
                }
            }
            assert!((ax - b0[i]).abs() < 1e-12);
        }
    }

    #[test]
    fn not_positive_definite() {
        let mut m = Skyline::default();
        m.set_structure(&[0, 0]);
        m.add(0, 0, 1.0);
        m.add(1, 0, 2.0);
        m.add(1, 1, 1.0);
        assert!(!m.cholesky());
    }

    #[test]
    fn rcm_is_permutation() {
        // path graph given scattered: 0-5-1-4-2-3
        let edges = [(0, 5), (5, 1), (1, 4), (4, 2), (2, 3)];
        let mut adj = vec![Vec::new(); 6];
        for (a, b) in edges {
            adj[a].push(b as u32);
            adj[b].push(a as u32);
        }
        let p = rcm(&adj);
        let mut s = p.clone();
        s.sort_unstable();
        assert_eq!(s, (0..6).collect::<Vec<u32>>());
        // bandwidth 1 in the new order
        let mut inv = [0usize; 6];
        for (new, &old) in p.iter().enumerate() {
            inv[old as usize] = new;
        }
        for (a, b) in edges {
            assert_eq!(inv[a].abs_diff(inv[b]), 1);
        }
    }
}
