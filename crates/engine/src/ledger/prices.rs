//! The catalog and the prices it gives each model over time.

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, params};

use super::changes::{Touch, next_revision, touch};
use super::{Ledger, instant};
use crate::catalog::{Catalog, Prices};
use crate::error::{Error, Result};
use crate::price::{PriceBook, Priced};
use crate::runtime::{CatalogCheck, CatalogOutcome};
use crate::time::Instant;

/// How long after a model's release a change to its prices is taken as a
/// correction of the catalog, applying to all its usage, rather than a change
/// of price from then on: fourteen days.
const CORRECTING: i64 = 14 * 24 * 60 * 60 * 1000;

/// How many checks of the catalog are kept.
const CHECKS_KEPT: i64 = 50;

/// The catalog last taken in, and where it came from.
pub(crate) struct Stored {
    /// The catalog.
    pub catalog: Catalog,
    /// Where it came from: `bundled` or `models.dev`.
    pub source: String,
    /// The entity tag models.dev gave it, to ask next time whether it changed.
    pub etag: Option<String>,
}

/// What taking in a catalog changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Taken {
    /// Models listed for the first time.
    pub added: usize,
    /// Models whose prices changed from some moment on.
    pub changed: usize,
    /// Models whose prices were corrected soon after their release, or that
    /// were priced for the first time, having been listed without prices:
    /// their new prices stand for all their usage.
    pub corrected: usize,
}

impl Ledger {
    /// The catalog last taken in, when there is one.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger cannot be read, and
    /// [`Error::Corrupt`] when the stored catalog cannot be.
    pub(crate) fn catalog(&self) -> Result<Option<Stored>> {
        let row = self
            .connection
            .query_row(
                "SELECT body, source, etag FROM catalog WHERE id = 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((body, source, etag)) = row else {
            return Ok(None);
        };
        let catalog = serde_json::from_slice(&body)
            .map_err(|error| Error::corrupt("stored catalog", error.to_string()))?;
        Ok(Some(Stored {
            catalog,
            source,
            etag,
        }))
    }

    /// Take in `catalog`, from `source`: record each model whose prices it
    /// changes, from when they changed, and keep it for what it says of
    /// models, with `etag`, to ask next time whether it changed. Returns
    /// what it changed.
    ///
    /// A catalog no newer than the one last taken in, as one fetched after
    /// the clock went back is, is not taken in and changes nothing: `None`.
    /// Its `etag`, when it has one, is kept all the same, so the same
    /// catalog isn't fetched again only to be left out again.
    ///
    /// A model's first prices stand for all its usage, including a model
    /// listed without prices before. A later change takes effect from the
    /// day models.dev dates the model's listing, but no earlier than the last
    /// catalog that still listed the old prices; within fourteen days of a
    /// model's release, a change is a correction and stands for all the
    /// model's usage. A model a catalog no longer lists, or lists without
    /// prices it can read, keeps its last prices.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger cannot be written, and
    /// [`Error::Corrupt`] when stored prices cannot be read.
    pub(crate) fn take_catalog(
        &mut self,
        catalog: &Catalog,
        source: &str,
        etag: Option<&str>,
    ) -> Result<Option<Taken>> {
        let as_of = catalog.as_of().map_or(0, Instant::millis);
        let previous = self.catalog_as_of()?;
        let mut taken = Taken::default();
        if previous.is_some_and(|previous| as_of <= previous) {
            if let Some(etag) = etag {
                self.connection
                    .execute("UPDATE catalog SET etag = ?1 WHERE id = 1", [etag])?;
            }
            return Ok(None);
        }
        let current = latest_prices(&self.connection)?;
        let transaction = self.connection.transaction()?;
        {
            let mut insert = transaction.prepare(
                "INSERT INTO price (provider, model, since, prices) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (provider, model, since) DO UPDATE SET prices = excluded.prices",
            )?;
            for (provider, id, model) in catalog.models() {
                // Written only for a model whose prices change, most stand.
                let stored = || stored_prices(&model.prices);
                let key = (provider.to_owned(), id.to_owned());
                let Some(Priced {
                    since,
                    prices: known,
                }) = current.get(&key)
                else {
                    insert.execute(params![provider, id, i64::MIN, stored()?])?;
                    taken.added += 1;
                    continue;
                };
                // A listing without prices, or with prices that don't read,
                // says nothing of what the model costs.
                if model.prices.is_none() || *known == model.prices {
                    continue;
                }
                let released = model.released.as_deref().and_then(Instant::from_date);
                let correcting = released
                    .is_some_and(|released| as_of < released.millis().saturating_add(CORRECTING));
                // A model's first prices are its prices from the start.
                if correcting || known.is_none() {
                    insert.execute(params![provider, id, since, stored()?])?;
                    taken.corrected += 1;
                    continue;
                }
                // No earlier than the last catalog that still listed the old
                // prices, and no later than this one.
                let after = previous.unwrap_or(i64::MIN).max(*since).saturating_add(1);
                let updated = model
                    .updated
                    .as_deref()
                    .and_then(Instant::from_date)
                    .map_or(as_of, Instant::millis);
                insert.execute(params![
                    provider,
                    id,
                    updated.clamp(after, as_of.max(after)),
                    stored()?
                ])?;
                taken.changed += 1;
            }
        }
        let body = catalog
            .to_json()
            .map_err(|error| Error::corrupt("catalog", error.to_string()))?;
        if taken != Taken::default() {
            let revision = next_revision(&transaction)?;
            touch(&transaction, revision, Touch::Prices, "", "")?;
        }
        transaction.execute(
            "INSERT INTO catalog (id, as_of, source, etag, body) VALUES (1, ?1, ?2, ?3, ?4)
             ON CONFLICT (id) DO UPDATE SET as_of = excluded.as_of, source = excluded.source,
                 etag = excluded.etag, body = excluded.body",
            params![as_of, source, etag, body],
        )?;
        transaction.commit()?;
        Ok(Some(taken))
    }

    /// When the catalog last taken in was published, in milliseconds; `None`
    /// before any.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger cannot be read.
    pub(crate) fn catalog_as_of(&self) -> Result<Option<i64>> {
        Ok(self
            .connection
            .query_row("SELECT as_of FROM catalog WHERE id = 1", [], |row| {
                row.get(0)
            })
            .optional()?)
    }

    /// Every model's prices over time.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger cannot be read, and
    /// [`Error::Corrupt`] when stored prices do not parse.
    pub(crate) fn price_book(&self) -> Result<PriceBook> {
        let mut statement = self
            .connection
            .prepare("SELECT provider, model, since, prices FROM price")?;
        let mut rows = statement.query([])?;
        let mut book = PriceBook::default();
        while let Some(row) = rows.next()? {
            let provider: String = row.get(0)?;
            let model: String = row.get(1)?;
            let prices = read_prices(row.get(3)?)?;
            book.insert(&provider, &model, row.get(2)?, prices);
        }
        Ok(book)
    }

    /// The latest check of the catalog. A check whose outcome this build
    /// doesn't know, as a newer one may have recorded, is passed over.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger cannot be read, and
    /// [`Error::Corrupt`] when a check's time is out of range.
    pub(crate) fn last_check(&self) -> Result<Option<CatalogCheck>> {
        let mut statement = self
            .connection
            .prepare_cached("SELECT at, outcome, detail FROM catalog_check ORDER BY at DESC")?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            if let Some(outcome) = CatalogOutcome::from_key(&row.get::<_, String>(1)?) {
                return Ok(Some(CatalogCheck {
                    at: instant(row.get(0)?, "catalog check time")?,
                    outcome,
                    detail: row.get(2)?,
                }));
            }
        }
        Ok(None)
    }

    /// Record `check` of the catalog, keeping the latest fifty.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger cannot be written.
    pub(crate) fn note_check(&mut self, check: &CatalogCheck) -> Result<()> {
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO catalog_check (at, outcome, detail) VALUES (?1, ?2, ?3)",
            params![check.at.millis(), check.outcome.key(), check.detail],
        )?;
        transaction.execute(
            "DELETE FROM catalog_check WHERE rowid NOT IN
                 (SELECT rowid FROM catalog_check ORDER BY at DESC LIMIT ?1)",
            [CHECKS_KEPT],
        )?;
        transaction.commit()?;
        Ok(())
    }
}

/// Every model's latest prices, and when they took effect, by provider and
/// model.
fn latest_prices(connection: &Connection) -> Result<HashMap<(String, String), Priced>> {
    let mut statement = connection.prepare(
        "SELECT provider, model, since, prices FROM price p
         WHERE since = (SELECT max(since) FROM price q WHERE q.provider = p.provider AND q.model = p.model)",
    )?;
    let mut rows = statement.query([])?;
    let mut latest = HashMap::new();
    while let Some(row) = rows.next()? {
        let key = (row.get::<_, String>(0)?, row.get::<_, String>(1)?);
        let priced = Priced {
            since: row.get(2)?,
            prices: read_prices(row.get(3)?)?,
        };
        latest.insert(key, priced);
    }
    Ok(latest)
}

/// Prices as the ledger stores them: JSON, or NULL for none.
fn stored_prices(prices: &Option<Prices>) -> Result<Option<String>> {
    prices
        .as_ref()
        .map(|prices| {
            serde_json::to_string(prices)
                .map_err(|error| Error::corrupt("prices", error.to_string()))
        })
        .transpose()
}

/// Stored prices, read back.
fn read_prices(stored: Option<String>) -> Result<Option<Prices>> {
    stored
        .map(|json| {
            serde_json::from_str(&json)
                .map_err(|error| Error::corrupt("stored prices", error.to_string()))
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::Taken;
    use crate::catalog::Catalog;
    use crate::ledger::Ledger;
    use crate::time::Instant;

    fn day(date: &str) -> Instant {
        Instant::from_date(date).unwrap()
    }

    /// A catalog of one Anthropic model costing `cost`, whose listing
    /// models.dev dates `updated`, read on `as_of`.
    fn listing(cost: Value, released: &str, updated: &str, as_of: &str) -> Catalog {
        let listing = json!({"anthropic": {"models": {"claude-opus-5": {
            "name": "Claude Opus 5", "release_date": released, "last_updated": updated,
            "cost": cost}}}});
        Catalog::parse(listing.to_string().as_bytes())
            .unwrap()
            .read_at(day(as_of))
    }

    /// A catalog of one Anthropic model at `output` dollars a million.
    fn catalog(output: f64, released: &str, updated: &str, as_of: &str) -> Catalog {
        let cost = json!({"input": 5, "output": output, "cache_read": 0.5, "cache_write": 6.25});
        listing(cost, released, updated, as_of)
    }

    fn output_rate(ledger: &Ledger, at: &str) -> Option<f64> {
        let book = ledger.price_book().unwrap();
        book.prices_at("anthropic", "claude-opus-5", Some(day(at)))
            .map(|prices| prices.base.output)
    }

    fn ledger() -> (tempfile::TempDir, Ledger) {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(&dir.path().join("ledger.sqlite")).unwrap();
        (dir, ledger)
    }

    #[test]
    fn a_price_change_takes_effect_from_the_day_models_dev_dates_it() {
        let (_dir, mut ledger) = ledger();
        let first = ledger.take_catalog(
            &catalog(25.0, "2026-07-24", "2026-07-24", "2026-08-01"),
            "bundled",
            None,
        );
        assert_eq!(
            first.unwrap(),
            Some(Taken {
                added: 1,
                changed: 0,
                corrected: 0
            })
        );
        // The first prices stand for all usage, however early.
        assert_eq!(output_rate(&ledger, "2020-01-01"), Some(25.0));

        let cut = catalog(20.0, "2026-07-24", "2026-09-10", "2026-09-20");
        assert_eq!(
            ledger
                .take_catalog(&cut, "models.dev", None)
                .unwrap()
                .map(|taken| taken.changed),
            Some(1)
        );
        assert_eq!(output_rate(&ledger, "2026-09-09"), Some(25.0));
        assert_eq!(output_rate(&ledger, "2026-09-10"), Some(20.0));
    }

    #[test]
    fn a_change_is_dated_no_earlier_than_the_last_catalog_with_the_old_price() {
        let (_dir, mut ledger) = ledger();
        ledger
            .take_catalog(
                &catalog(25.0, "2026-07-24", "2026-07-24", "2026-09-01"),
                "bundled",
                None,
            )
            .unwrap();
        // models.dev dates the listing 2026-08-15, but the catalog read on
        // 2026-09-01 still had the old price, so the new one runs from then.
        let cut = catalog(20.0, "2026-07-24", "2026-08-15", "2026-09-20");
        ledger.take_catalog(&cut, "models.dev", None).unwrap();
        assert_eq!(output_rate(&ledger, "2026-08-20"), Some(25.0));
        assert_eq!(output_rate(&ledger, "2026-09-02"), Some(20.0));
    }

    #[test]
    fn a_change_soon_after_release_corrects_the_price_for_all_usage() {
        let (_dir, mut ledger) = ledger();
        ledger
            .take_catalog(
                &catalog(250.0, "2026-09-15", "2026-09-15", "2026-09-16"),
                "bundled",
                None,
            )
            .unwrap();
        let fixed = catalog(25.0, "2026-09-15", "2026-09-18", "2026-09-20");
        assert_eq!(
            ledger
                .take_catalog(&fixed, "models.dev", None)
                .unwrap()
                .map(|taken| taken.corrected),
            Some(1)
        );
        assert_eq!(output_rate(&ledger, "2026-09-15"), Some(25.0));
    }

    #[test]
    fn a_model_listed_before_it_was_priced_takes_its_first_prices_for_all_usage() {
        let (_dir, mut ledger) = ledger();
        // Listed on 2026-08-01, weeks after its release, without prices.
        ledger
            .take_catalog(
                &listing(json!({}), "2026-07-24", "2026-07-24", "2026-08-01"),
                "bundled",
                None,
            )
            .unwrap();
        assert_eq!(output_rate(&ledger, "2026-08-02"), None);

        let priced = catalog(25.0, "2026-07-24", "2026-09-10", "2026-09-20");
        assert_eq!(
            ledger.take_catalog(&priced, "models.dev", None).unwrap(),
            Some(Taken {
                added: 0,
                changed: 0,
                corrected: 1
            })
        );
        assert_eq!(output_rate(&ledger, "2026-08-02"), Some(25.0));
    }

    #[test]
    fn a_catalog_fetched_after_the_clock_went_back_is_not_taken_in_but_its_tag_is_kept() {
        let (_dir, mut ledger) = ledger();
        ledger
            .take_catalog(
                &catalog(25.0, "2026-07-24", "2026-07-24", "2026-09-20"),
                "models.dev",
                Some("\"a\""),
            )
            .unwrap();
        // The clock went back a day, and models.dev has a new price: the
        // catalog is dated before the one kept, so it changes nothing.
        let behind = catalog(20.0, "2026-07-24", "2026-09-18", "2026-09-19");
        assert_eq!(
            ledger
                .take_catalog(&behind, "models.dev", Some("\"b\""))
                .unwrap(),
            None
        );
        assert_eq!(output_rate(&ledger, "2026-09-21"), Some(25.0));
        // What models.dev answered is known, so it isn't asked for again
        // until it changes.
        let kept = ledger.catalog().unwrap().unwrap();
        assert_eq!(kept.etag.as_deref(), Some("\"b\""));
        assert_eq!(
            ledger.catalog_as_of().unwrap(),
            Some(day("2026-09-20").millis())
        );
    }

    #[test]
    fn an_older_catalog_changes_nothing_and_a_model_unlisted_or_unpriced_keeps_its_prices() {
        let (_dir, mut ledger) = ledger();
        ledger
            .take_catalog(
                &catalog(20.0, "2026-07-24", "2026-09-10", "2026-09-20"),
                "models.dev",
                None,
            )
            .unwrap();
        let older = catalog(25.0, "2026-07-24", "2026-07-24", "2026-08-01");
        assert_eq!(ledger.take_catalog(&older, "bundled", None).unwrap(), None);
        assert_eq!(output_rate(&ledger, "2026-09-21"), Some(20.0));

        // No longer listed; listed without prices; listed with an input rate
        // below zero, which isn't a price.
        let empty = Catalog::parse(b"{}").unwrap().read_at(day("2026-09-30"));
        let unpriced = listing(json!({}), "2026-07-24", "2026-10-05", "2026-10-10");
        let unreadable = listing(
            json!({"input": -5, "output": 20}),
            "2026-07-24",
            "2026-10-15",
            "2026-10-20",
        );
        for (catalog, after) in [
            (empty, "2026-10-01"),
            (unpriced, "2026-10-11"),
            (unreadable, "2026-10-21"),
        ] {
            assert_eq!(
                ledger.take_catalog(&catalog, "models.dev", None).unwrap(),
                Some(Taken::default())
            );
            assert_eq!(output_rate(&ledger, after), Some(20.0));
        }
    }
}
