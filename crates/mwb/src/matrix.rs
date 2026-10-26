//! Machine matrix and neighbour lookup (Core/MachineStuff.cs `MoveLeft/Right/Up/Down`).
//!
//! The matrix has 4 slots. In one-row mode they are laid out left→right
//! `[0][1][2][3]`; in two-row mode as
//! ```text
//! [0][1]
//! [2][3]
//! ```
//! "Circle" (wrap-around, `MatrixSwapFlag` on the wire) lets the cursor leave one
//! end and enter the other.

use serde::{Deserialize, Serialize};

pub const MAX_MACHINE: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Matrix {
    /// Up to 4 machine names; empty string = empty slot.
    pub slots: [String; MAX_MACHINE],
    /// `MatrixOneRow` (default true in MWB).
    pub one_row: bool,
    /// `MatrixCircle`: wrap-around.
    pub circle: bool,
}

impl Default for Matrix {
    fn default() -> Self {
        Matrix { slots: Default::default(), one_row: true, circle: false }
    }
}

fn eq(a: &str, b: &str) -> bool {
    !b.trim().is_empty() && a.trim().eq_ignore_ascii_case(b.trim())
}

impl Matrix {
    pub fn from_names<S: AsRef<str>>(names: &[S], one_row: bool, circle: bool) -> Self {
        let mut m = Matrix { one_row, circle, ..Default::default() };
        for (i, n) in names.iter().take(MAX_MACHINE).enumerate() {
            m.slots[i] = n.as_ref().trim().to_string();
        }
        m
    }

    pub fn contains(&self, name: &str) -> bool {
        self.slots.iter().any(|s| eq(name, s))
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.slots.iter().map(|s| s.trim()).filter(|s| !s.is_empty())
    }

    /// Find the machine id the cursor moves to when leaving `current` in `dir`.
    ///
    /// * `resolve(name)` — machine id known for that name (C# `IdFromName`, `None` = `ID.NONE`).
    /// * `connected(name)` — whether there is a live connection (only used in one-row
    ///   mode, which C# filters through `LiveMachineMatrix`).
    pub fn neighbour(
        &self,
        current: &str,
        dir: Dir,
        resolve: &dyn Fn(&str) -> Option<u32>,
        connected: &dyn Fn(&str) -> bool,
    ) -> Option<u32> {
        let mc: Vec<String> = if self.one_row {
            self.slots
                .iter()
                .map(|m| if !m.trim().is_empty() && connected(m.trim()) { m.trim().to_string() } else { String::new() })
                .collect()
        } else {
            self.slots.iter().map(|m| m.trim().to_string()).collect()
        };
        let nonempty = |i: usize| !mc[i].is_empty();
        let id = |i: usize| resolve(&mc[i]).filter(|&v| v != 0);

        if self.one_row {
            match dir {
                Dir::Up | Dir::Down => None,
                Dir::Right => {
                    let i = (0..MAX_MACHINE).find(|&i| eq(current, &mc[i]))?;
                    for j in i..MAX_MACHINE - 1 {
                        if nonempty(j + 1) {
                            if let Some(v) = id(j + 1) {
                                return Some(v);
                            }
                        }
                    }
                    if self.circle {
                        for j in 0..i {
                            if nonempty(j) {
                                if let Some(v) = id(j) {
                                    return Some(v);
                                }
                            }
                        }
                    }
                    None
                }
                Dir::Left => {
                    let i = (0..MAX_MACHINE).rev().find(|&i| eq(current, &mc[i]))?;
                    for j in (1..=i).rev() {
                        if nonempty(j - 1) {
                            if let Some(v) = id(j - 1) {
                                return Some(v);
                            }
                        }
                    }
                    if self.circle {
                        for j in ((i + 1)..MAX_MACHINE).rev() {
                            if nonempty(j) {
                                if let Some(v) = id(j) {
                                    return Some(v);
                                }
                            }
                        }
                    }
                    None
                }
            }
        } else {
            // (current slot, target slot, needs circle)
            let rules: &[(usize, usize, bool)] = match dir {
                Dir::Right => &[(0, 1, false), (2, 3, false), (1, 0, true), (3, 2, true)],
                Dir::Left => &[(1, 0, false), (3, 2, false), (0, 1, true), (2, 3, true)],
                Dir::Up => &[(2, 0, false), (3, 1, false), (0, 2, true), (1, 3, true)],
                Dir::Down => &[(0, 2, false), (1, 3, false), (2, 0, true), (3, 1, true)],
            };
            for &(from, to, circ) in rules {
                if circ && !self.circle {
                    continue;
                }
                if eq(current, &mc[from]) && nonempty(to) {
                    // C#: first matching branch wins even if the id does not resolve.
                    return id(to);
                }
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(n: &str) -> Option<u32> {
        match n.to_ascii_uppercase().as_str() {
            "A" => Some(1),
            "B" => Some(2),
            "C" => Some(3),
            "D" => Some(4),
            _ => None,
        }
    }

    #[test]
    fn one_row() {
        let m = Matrix::from_names(&["A", "B", "", "C"], true, false);
        let all = |_: &str| true;
        assert_eq!(m.neighbour("a", Dir::Right, &ids, &all), Some(2));
        assert_eq!(m.neighbour("B", Dir::Right, &ids, &all), Some(3));
        assert_eq!(m.neighbour("C", Dir::Right, &ids, &all), None);
        assert_eq!(m.neighbour("C", Dir::Left, &ids, &all), Some(2));
        assert_eq!(m.neighbour("A", Dir::Left, &ids, &all), None);
        assert_eq!(m.neighbour("A", Dir::Up, &ids, &all), None);
        let m = Matrix { circle: true, ..m };
        assert_eq!(m.neighbour("C", Dir::Right, &ids, &all), Some(1));
        assert_eq!(m.neighbour("A", Dir::Left, &ids, &all), Some(3));
        // disconnected machines are skipped in one-row mode
        let not_b = |n: &str| n != "B";
        assert_eq!(m.neighbour("A", Dir::Right, &ids, &not_b), Some(3));
    }

    #[test]
    fn two_row() {
        let all = |_: &str| true;
        let m = Matrix::from_names(&["A", "B", "C", "D"], false, false);
        assert_eq!(m.neighbour("A", Dir::Right, &ids, &all), Some(2));
        assert_eq!(m.neighbour("A", Dir::Down, &ids, &all), Some(3));
        assert_eq!(m.neighbour("D", Dir::Up, &ids, &all), Some(2));
        assert_eq!(m.neighbour("D", Dir::Left, &ids, &all), Some(3));
        assert_eq!(m.neighbour("B", Dir::Right, &ids, &all), None);
        let m = Matrix { circle: true, ..m };
        assert_eq!(m.neighbour("B", Dir::Right, &ids, &all), Some(1));
        assert_eq!(m.neighbour("C", Dir::Down, &ids, &all), Some(1));
    }
}
