//! `turnscope doctor`: read every agent's history into the ledger and report
//! what it found: how much each agent's files held, what could not be made
//! sense of, how the transcripts compare with the agents' own totals, and how
//! the prices compare with the agents' own costs.
//!
//! The report is for comparing one run with another, after a change to a
//! reader, to merging or to pricing, so its counts are shortened to four
//! figures, finer than the three the app shows.

use std::fmt::Write as _;
use std::path::Path;

use turnscope_engine::{
    CatalogOutcome, Check, Counts, Diagnostic, Doctor, Engine, Pricing, ScanReport, Usd,
};

use crate::{Failure, show};

/// Read every agent's history, and report what the ledger holds and how it
/// compares with the agents' own totals.
///
/// `strict`, it fails when any artifact could not be read or anything read
/// was not understood, for a script to act on.
pub(crate) fn run(data: &Path, home: &Path, strict: bool) -> Result<(), Failure> {
    let engine = Engine::open(data, home)?;
    let scan = engine.scan()?;
    let report = engine.doctor()?;
    let mut out = String::new();
    write_scan(&mut out, &scan);
    write_doctor(&mut out, &report);
    show(&out)?;
    let unread = scan.failed.len();
    // Usage an agent never recorded is understood, and listed apart.
    let unknown = report
        .agents
        .iter()
        .flat_map(|agent| &agent.diagnostics)
        .filter(|diagnostic| diagnostic.kind.misunderstood())
        .count();
    if strict && unread + unknown > 0 {
        return Err(Failure::Failed(format!(
            "{unread} artifacts could not be read, and {unknown} kinds of things read were not \
             understood"
        )));
    }
    Ok(())
}

fn write_scan(out: &mut String, scan: &ScanReport) {
    let _ = writeln!(
        out,
        "Read {} new, {} resumed, {} again from the start; {} unchanged, {} gone. {} in {:.2} s.",
        scan.new,
        scan.resumed,
        scan.reread,
        scan.unchanged,
        scan.absent,
        bytes(scan.bytes),
        scan.elapsed.as_secs_f64()
    );
    for (path, error) in &scan.failed {
        let _ = writeln!(out, "  could not read {}: {error}", path.display());
    }
}

fn write_doctor(out: &mut String, report: &Doctor) {
    let prices = &report.prices;
    let _ = writeln!(
        out,
        "Prices from the {} catalog of {}.",
        prices.source.as_deref().unwrap_or("unknown"),
        prices
            .as_of
            .map_or_else(|| "an unknown date".to_owned(), |at| at.to_string())
    );
    if let Some(check) = &prices.checked {
        let found = match check.outcome {
            CatalogOutcome::Unchanged => "unchanged",
            CatalogOutcome::Updated => "updated",
            CatalogOutcome::Refused => "refused",
            CatalogOutcome::Unreachable => "unreachable",
        };
        let _ = writeln!(
            out,
            "Last checked models.dev at {}: {found} {}",
            check.at, check.detail
        );
    }
    for agent in &report.agents {
        let _ = writeln!(out);
        let _ = writeln!(out, "{}", agent.agent);
        let _ = writeln!(
            out,
            "  {} files ({} gone) · {} sessions · {} responses",
            agent.artifacts, agent.absent, agent.sessions, agent.responses
        );
        let tokens = &agent.tokens;
        let _ = writeln!(
            out,
            "  input {} · cache read {} · cache write {} (1h {}) · output {} (reasoning {}) · web searches {}",
            short(tokens.input),
            short(tokens.cache_read),
            short(tokens.cache_write()),
            short(tokens.cache_write_1h),
            short(tokens.output),
            short(tokens.reasoning),
            agent.web_searches
        );
        let checks: Vec<&Check> = report
            .checks
            .iter()
            .filter(|check| check.session.agent() == agent.agent)
            .collect();
        if !checks.is_empty() {
            write_checks(out, agent.agent.name(), &checks);
        }
        if let Some(pricing) = report
            .pricing
            .iter()
            .find(|pricing| pricing.agent == agent.agent)
        {
            write_pricing(out, pricing);
        }
        for (heading, misunderstood) in [
            ("Usage never recorded, so unknown:", false),
            ("Not understood:", true),
        ] {
            let listed: Vec<&Diagnostic> = agent
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.kind.misunderstood() == misunderstood)
                .collect();
            if listed.is_empty() {
                continue;
            }
            let _ = writeln!(out);
            let _ = writeln!(out, "  {heading}");
            for diagnostic in listed {
                let _ = writeln!(
                    out,
                    "    {:<15} {} — {} times in {} files, first at {}:{}",
                    diagnostic.kind.key(),
                    diagnostic.detail,
                    diagnostic.count,
                    diagnostic.artifacts,
                    diagnostic.example.display(),
                    diagnostic.offset
                );
            }
        }
    }
}

fn write_checks(out: &mut String, agent: &str, checks: &[&Check]) {
    let mut sessions: Vec<&str> = checks.iter().map(|check| check.session.native()).collect();
    sessions.sort_unstable();
    sessions.dedup();
    let mut reported = Counts::default();
    let mut observed = Counts::default();
    for check in checks {
        reported.add(check.reported);
        observed.add(check.observed);
    }
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "  Against {agent}'s own totals, {} sessions:",
        sessions.len()
    );
    let _ = writeln!(
        out,
        "    {:<12} {:>14} {:>16} {:>18}",
        "", "it counted", "transcripts show", "outside them"
    );
    for (name, said, shown) in [
        ("input", reported.input, observed.input),
        ("cache read", reported.cache_read, observed.cache_read),
        ("cache write", reported.cache_write, observed.cache_write),
        ("output", reported.output, observed.output),
    ] {
        let _ = writeln!(
            out,
            "    {:<12} {:>14} {:>16} {:>18}",
            name,
            short(said),
            short(shown),
            outside(said, shown)
        );
    }

    let missing = |check: &Check| {
        check
            .reported
            .cache_read
            .saturating_sub(check.observed.cache_read)
    };
    let mut widest: Vec<&&Check> = checks.iter().filter(|check| missing(check) > 0).collect();
    widest.sort_by_key(|check| std::cmp::Reverse(missing(check)));
    if !widest.is_empty() {
        let _ = writeln!(out, "  Largest differences in cache reads:");
    }
    for check in widest.iter().take(5) {
        let _ = writeln!(
            out,
            "    {} {:<28} counted {:>9}, shown {:>9} across {} sessions",
            check.session,
            check
                .model
                .as_ref()
                .map_or("all models", |model| model.as_str()),
            short(check.reported.cache_read),
            short(check.observed.cache_read),
            check.sessions.len()
        );
    }
    let over: Vec<&&Check> = checks
        .iter()
        .filter(|check| {
            check.observed.input > check.reported.input
                || check.observed.cache_read > check.reported.cache_read
                || check.observed.cache_write > check.reported.cache_write
                || check.observed.output > check.reported.output
        })
        .collect();
    if !over.is_empty() {
        let _ = writeln!(
            out,
            "  Transcripts showing more than {agent} counted ({}):",
            over.len()
        );
        for check in over.iter().take(10) {
            let _ = writeln!(
                out,
                "    {} {:<28} input {}/{} · cache read {}/{} · cache write {}/{} · output {}/{}",
                check.session,
                check
                    .model
                    .as_ref()
                    .map_or("all models", |model| model.as_str()),
                short(check.observed.input),
                short(check.reported.input),
                short(check.observed.cache_read),
                short(check.reported.cache_read),
                short(check.observed.cache_write),
                short(check.reported.cache_write),
                short(check.observed.output),
                short(check.reported.output)
            );
        }
    }
}

fn write_pricing(out: &mut String, pricing: &Pricing) {
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "  Cost: {} from catalog prices ({} of it approximate), {} charged, {} the agent's own estimate",
        money(pricing.catalog),
        money(pricing.approximate),
        money(pricing.charged),
        money(pricing.estimated)
    );
    for model in pricing.unpriced.iter().take(5) {
        // Usage whose model its agent hasn't named, which has no price.
        let named = if model.model.is_empty() {
            " (no model named)".to_owned()
        } else {
            format!("/{}", model.model)
        };
        let _ = writeln!(
            out,
            "    unpriced: {}{named} — {} responses, {} tokens",
            model.provider,
            model.responses,
            short(model.tokens)
        );
    }
    if let Some(comparison) = &pricing.comparison {
        let _ = writeln!(
            out,
            "  Against the agent's own costs: {} compared, {} within 1%; it says {}, catalog prices say {}",
            comparison.compared,
            comparison.agreeing,
            money(comparison.recorded),
            money(comparison.ours)
        );
        for difference in &comparison.furthest {
            let _ = writeln!(
                out,
                "    {} — it says {}, catalog prices say {}",
                difference.what,
                money(difference.recorded),
                money(difference.ours)
            );
        }
    }
}

/// An amount in dollars.
fn money(amount: Usd) -> String {
    format!("${:.2}", amount.dollars())
}

/// How much of what the agent counted the transcripts do not show.
fn outside(said: u64, shown: u64) -> String {
    let missing = said.saturating_sub(shown);
    if said == 0 {
        return short(missing);
    }
    // Rounding counts this large to `f64` is far below the one decimal place
    // shown.
    let percent = missing as f64 * 100.0 / said as f64;
    format!("{} ({percent:.1}%)", short(missing))
}

/// A count in thousands, millions or billions, to four figures. Each unit
/// takes over where the one below would round to a thousand of it, so
/// 999,950 reads 1.00M, not 1000.0K.
fn short(count: u64) -> String {
    // Counts past 2^53 lose a little precision as f64, far below the figures
    // shown.
    let value = count as f64;
    match count {
        0..1_000 => count.to_string(),
        1_000..999_950 => format!("{:.1}K", value / 1e3),
        999_950..999_995_000 => format!("{:.2}M", value / 1e6),
        _ => format!("{:.2}B", value / 1e9),
    }
}

/// A size in bytes. Each unit takes over where the one below would round to
/// a thousand of it, so 999,500 bytes read 1 MB, not 1000 KB.
fn bytes(size: u64) -> String {
    let value = size as f64;
    match size {
        0..999_500 => format!("{:.0} KB", value / 1e3),
        999_500..999_500_000 => format!("{:.0} MB", value / 1e6),
        _ => format!("{:.2} GB", value / 1e9),
    }
}

#[cfg(test)]
mod tests {
    use super::{bytes, short};

    #[test]
    fn a_count_moves_up_a_unit_where_it_would_round_to_a_thousand() {
        assert_eq!(short(999), "999");
        assert_eq!(short(1_000), "1.0K");
        // 999,949 is 999.949 thousand, 999.9K to a tenth; 999,950 would be
        // 1000.0K, so it is 0.99995 million, 1.00M.
        assert_eq!(short(999_949), "999.9K");
        assert_eq!(short(999_950), "1.00M");
        // 999,994,999 is 999.994999 million, 999.99M; 999,995,000 would be
        // 1000.00M, so it is 1.00B.
        assert_eq!(short(999_994_999), "999.99M");
        assert_eq!(short(999_995_000), "1.00B");
        assert_eq!(short(37_623_456_789), "37.62B");
    }

    #[test]
    fn a_size_moves_up_a_unit_where_it_would_round_to_a_thousand() {
        // 999,499 bytes are 999.499 KB, 999 KB whole; 999,500 would be
        // 1000 KB, so they are 0.9995 MB, 1 MB.
        assert_eq!(bytes(999_499), "999 KB");
        assert_eq!(bytes(999_500), "1 MB");
        // 999,499,999 bytes are 999.499999 MB, 999 MB; 999,500,000 would be
        // 1000 MB, so they are 1.00 GB.
        assert_eq!(bytes(999_499_999), "999 MB");
        assert_eq!(bytes(999_500_000), "1.00 GB");
        assert_eq!(bytes(6_970_000_000), "6.97 GB");
    }
}
