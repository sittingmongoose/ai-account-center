//! Where each card goes: as many columns as the width allows, the cards in reading order (Update apps, the providers,
//! the settings), cut into columns so the tallest column is as short as it can be. Heights are each card's measured
//! lasting height (without a flow, a line or a form that opens while you use it; accounts/mod.rs), or an estimate
//! before the first measure, so the columns hold still while you work. A new cut is taken only when it is clearly
//! better than the one on screen.

use super::model::AccountsView;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum CardKey {
    Update,
    Provider(String),
    Signin,
    Settings,
    Cleanup,
    Logs,
    About,
}

impl CardKey {
    /// The card's name on its slot (`data-card`), for measuring.
    pub fn name(&self) -> String {
        match self {
            CardKey::Update => "update".into(),
            CardKey::Provider(id) => format!("provider:{id}"),
            CardKey::Signin => "signin".into(),
            CardKey::Settings => "settings".into(),
            CardKey::Cleanup => "cleanup".into(),
            CardKey::Logs => "logs".into(),
            CardKey::About => "about".into(),
        }
    }
}

/// The cards with their measured heights where known, else the estimates.
pub fn with_measured(
    mut cards: Vec<(CardKey, f64)>,
    measured: &std::collections::HashMap<String, f64>,
) -> Vec<(CardKey, f64)> {
    for (key, h) in cards.iter_mut() {
        if let Some(m) = measured.get(&key.name()) {
            *h = *m;
        }
    }
    cards
}

/// The tallest column of a cut, by these heights; None when the cut doesn't hold exactly these cards in order.
pub fn tallest(cols: &[Vec<CardKey>], cards: &[(CardKey, f64)]) -> Option<f64> {
    let flat: Vec<&CardKey> = cols.iter().flatten().collect();
    if flat.len() != cards.len() || flat.iter().zip(cards).any(|(a, (b, _))| *a != b) {
        return None;
    }
    let mut i = 0;
    let mut most: f64 = 0.0;
    for col in cols {
        let h: f64 = cards[i..i + col.len()].iter().map(|(_, h)| h + 16.0).sum();
        i += col.len();
        most = most.max(h);
    }
    Some(most)
}

/// The cut to show: the best one, unless the one on screen has as many columns and is within `slack` px of it.
pub fn choose(
    prev: Option<&Vec<Vec<CardKey>>>,
    cards: &[(CardKey, f64)],
    width: f64,
) -> Vec<Vec<CardKey>> {
    let best = columns(cards, width);
    if let (Some(prev), Some(best_h)) = (prev, tallest(&best, cards)) {
        if prev.len() == best.len() {
            if let Some(prev_h) = tallest(prev, cards) {
                if prev_h <= best_h + 60.0 {
                    return prev.clone();
                }
            }
        }
    }
    best
}

/// How many columns fit: 420 px each on desktop widths, 310 px below (two on the Duo open, one on phones), at most 5.
pub fn column_count(width: f64) -> usize {
    if width <= 0.0 {
        return 1;
    }
    let (min, gap) = if width >= 1200.0 {
        (420.0, 20.0)
    } else {
        (310.0, 10.0)
    };
    (((width + gap) / (min + gap)).floor() as usize).clamp(1, 5)
}

/// The width one column gets.
pub fn column_width(width: f64) -> f64 {
    let n = column_count(width) as f64;
    let gap = if width >= 1200.0 { 20.0 } else { 10.0 };
    ((width - gap * (n - 1.0)) / n).max(0.0)
}

/// Every card in reading order with its expected height in px at the column width it will get.
pub fn cards(view: &AccountsView, width: f64) -> Vec<(CardKey, f64)> {
    let col = column_width(width);
    // A row is one line pair in a wide card, two in a medium one, three or four in a narrow one.
    let row_h = if col >= 720.0 {
        52.0
    } else if col >= 440.0 {
        58.0
    } else {
        86.0
    };
    let mut out = Vec::new();
    // Update apps: the summary, and one line per app while its table is open (open by default on wide pages).
    let apps = update_apps(view);
    let update_open = width >= 900.0;
    let update_h = if !view.update.shown {
        78.0
    } else if update_open {
        96.0 + apps as f64 * 30.0
    } else {
        86.0
    };
    out.push((CardKey::Update, update_h));
    for p in view.col_a.iter().chain(view.col_b.iter()) {
        let mut h = if col >= 440.0 { 64.0 } else { 92.0 };
        h += p.rows.len() as f64 * row_h;
        if p.rows.is_empty() {
            h += 40.0;
        }
        if !p.trash.is_empty() {
            h += 34.0 + p.trash.len() as f64 * 50.0;
        }
        if p.ag {
            h += if col >= 520.0 { 120.0 } else { 220.0 };
        }
        h += if col >= 440.0 { 52.0 } else { 84.0 };
        out.push((CardKey::Provider(p.id.clone()), h));
    }
    let narrow = col < 440.0;
    out.push((CardKey::Signin, if narrow { 980.0 } else { 860.0 }));
    out.push((
        CardKey::Settings,
        250.0 + view.policies.len() as f64 * if narrow { 84.0 } else { 52.0 },
    ));
    out.push((CardKey::Cleanup, 150.0));
    out.push((
        CardKey::Logs,
        300.0 + view.log_sources.sources.len() as f64 * 54.0,
    ));
    out.push((CardKey::About, 104.0));
    out
}

/// The apps the Update apps table lists (one line each).
pub fn update_apps(view: &AccountsView) -> usize {
    let mut apps: Vec<&str> = Vec::new();
    for host in &view.update.hosts {
        for item in &host.items {
            if super::update::app_row(&item.key) && !apps.contains(&item.app.as_str()) {
                apps.push(&item.app);
            }
        }
    }
    apps.len()
}

/// The cards cut into columns in reading order, so the tallest column is as short as it can be (a linear partition).
pub fn columns(cards: &[(CardKey, f64)], width: f64) -> Vec<Vec<CardKey>> {
    let n = column_count(width).min(cards.len().max(1));
    if n <= 1 {
        return vec![cards.iter().map(|(k, _)| k.clone()).collect()];
    }
    let m = cards.len();
    let gap = 16.0;
    let mut prefix = vec![0.0; m + 1];
    for (i, (_, h)) in cards.iter().enumerate() {
        prefix[i + 1] = prefix[i] + h + gap;
    }
    let span = |a: usize, b: usize| prefix[b] - prefix[a];
    // best[k][i]: the smallest tallest column for the first i cards in k columns; cut[k][i]: where the last one starts.
    let mut best = vec![vec![f64::INFINITY; m + 1]; n + 1];
    let mut cut = vec![vec![0usize; m + 1]; n + 1];
    best[0][0] = 0.0;
    for k in 1..=n {
        for i in 0..=m {
            for j in 0..=i {
                let cost = best[k - 1][j].max(span(j, i));
                if cost < best[k][i] {
                    best[k][i] = cost;
                    cut[k][i] = j;
                }
            }
        }
    }
    let mut bounds = Vec::new();
    let mut i = m;
    for k in (1..=n).rev() {
        let j = cut[k][i];
        bounds.push((j, i));
        i = j;
    }
    bounds.reverse();
    bounds
        .into_iter()
        .filter(|(a, b)| b > a)
        .map(|(a, b)| cards[a..b].iter().map(|(k, _)| k.clone()).collect())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(n: usize) -> Vec<(CardKey, f64)> {
        (0..n)
            .map(|i| (CardKey::Provider(format!("p{i}")), 100.0))
            .collect()
    }

    #[test]
    fn every_pose_gets_its_column_count() {
        // iPhone 17 Pro, Duo folded beside the rail, Duo folded on its side, Duo open upright and on its side,
        // a 1024 tablet, and desktops at 1280, 1440, 1920 and 2560 (the page's inner widths).
        let cases = [
            (386.0, 1),
            (366.0, 1),
            (578.0, 1),
            (645.0, 2),
            (843.0, 2),
            (1000.0, 3),
            (1216.0, 2),
            (1376.0, 3),
            (1856.0, 4),
            (2496.0, 5),
        ];
        for (w, n) in cases {
            assert_eq!(column_count(w), n, "width {w}");
        }
    }

    #[test]
    fn columns_keep_reading_order_and_balance() {
        let cards = keys(9);
        let cols = columns(&cards, 1376.0);
        assert_eq!(cols.len(), 3);
        assert!(cols.iter().all(|c| c.len() == 3));
        let flat: Vec<CardKey> = cols.into_iter().flatten().collect();
        let order: Vec<CardKey> = cards.into_iter().map(|(k, _)| k).collect();
        assert_eq!(flat, order);
    }

    #[test]
    fn one_tall_card_gets_its_own_column() {
        let mut cards = keys(6);
        cards.insert(3, (CardKey::Signin, 600.0));
        let cols = columns(&cards, 1376.0);
        assert!(cols.iter().any(|c| c == &vec![CardKey::Signin]));
    }

    #[test]
    fn a_cut_on_screen_stays_unless_a_new_one_is_clearly_better() {
        let mut cards = keys(6);
        let first = columns(&cards, 1376.0);
        // a card grows a little: the cut on screen stays
        cards[0].1 = 140.0;
        assert_eq!(choose(Some(&first), &cards, 1376.0), first);
        // a card grows a lot: the columns are cut again
        cards[0].1 = 600.0;
        assert_ne!(choose(Some(&first), &cards, 1376.0), first);
    }

    #[test]
    fn a_phone_stacks_every_card() {
        let cols = columns(&keys(5), 386.0);
        assert_eq!(cols.len(), 1);
        assert_eq!(cols[0].len(), 5);
    }
}
