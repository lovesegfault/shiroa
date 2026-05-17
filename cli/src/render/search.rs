use std::{collections::BTreeMap, path::Path, sync::Mutex};

use elasticlunr::{Index, IndexBuilder};
use reflexo_typst::{error::prelude::*, path::unix_slash};
use serde::Serialize;
use typst::ecow::EcoString;

use crate::{
    book::meta::Search,
    utils::{collapse_whitespace, write_file},
};

const MAX_WORD_LENGTH_TO_INDEX: usize = 80;

/// Tokenizes in the same way as elasticlunr-rs (for English), but also drops
/// long tokens.
fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || c == '-')
        .filter(|s| !s.is_empty())
        .map(|s| s.trim().to_lowercase())
        .filter(|s| s.len() <= MAX_WORD_LENGTH_TO_INDEX)
        .collect()
}

/// Stores [`SearchItem`]s keyed by chapter URL so that `serve` can re-index
/// only the chapters it recompiled (replace-by-key) and still emit a full
/// index from the merged set. The elasticlunr [`Index`] is append-only, so
/// it's rebuilt fresh from `items` each time [`render_search_index`] is
/// called rather than kept as long-lived mutable state.
pub struct SearchRenderer {
    items: BTreeMap<String, SearchItem>,
    pub config: Search,
}

impl Default for SearchRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchRenderer {
    pub fn new() -> Self {
        SearchRenderer {
            items: BTreeMap::new(),
            config: Search::default(),
        }
    }

    /// Insert or replace the entry for each item's chapter URL.
    pub fn merge(&mut self, items: Vec<SearchItem>) {
        for item in items {
            self.items.insert(item.anchor_base.clone(), item);
        }
    }

    pub fn render_search_index(&self, dest_dir: &Path) -> Result<()> {
        let mut index = IndexBuilder::new()
            .add_field_with_tokenizer("title", Box::new(&tokenize))
            .add_field_with_tokenizer("body", Box::new(&tokenize))
            .add_field_with_tokenizer("breadcrumbs", Box::new(&tokenize))
            .build();
        let mut doc_urls: Vec<String> = Vec::with_capacity(self.items.len());

        for item in self.items.values() {
            let doc_ref = doc_urls.len().to_string();
            doc_urls.push(item.anchor_base.clone());

            let title = item.title.as_str();
            let desc = item.desc.as_deref().unwrap_or("");
            // , &breadcrumbs.join(" » ")
            // todo: currently, breadcrumbs is title it self
            let fields = [title, desc, title];
            index.add_doc(
                &doc_ref,
                fields.iter().map(|&x| collapse_whitespace(x.trim())),
            );
        }

        let json = write_to_json(&index, &self.config, &doc_urls)?;
        if json.len() > 10_000_000 {
            log::warn!("searchindex.json is very large ({} bytes)", json.len());
        }

        write_file(dest_dir.join("searchindex.json"), json.as_bytes())?;
        write_file(
            dest_dir.join("searchindex.js"),
            format!("Object.assign(window.search, {json});").as_bytes(),
        )?;

        Ok(())
    }
}

pub struct SearchItem {
    anchor_base: String,
    title: EcoString,
    desc: Option<EcoString>,
}

pub struct SearchCtx<'a> {
    pub config: &'a Search,
    pub items: Mutex<Vec<SearchItem>>,
}

impl SearchCtx<'_> {
    pub fn index_search(&self, dest: &Path, title: EcoString, desc: Option<EcoString>) {
        let anchor_base = unix_slash(dest);

        self.items.lock().unwrap().push(SearchItem {
            anchor_base,
            title,
            desc,
        });
    }
}

fn write_to_json(index: &Index, search_config: &Search, doc_urls: &Vec<String>) -> Result<String> {
    use std::collections::BTreeMap;

    use elasticlunr::config::{SearchBool, SearchOptions, SearchOptionsField};

    #[derive(Serialize)]
    struct ResultsOptions {
        limit_results: u32,
        teaser_word_count: u32,
    }

    #[derive(Serialize)]
    struct SearchindexJson<'a> {
        /// The options used for displaying search results
        results_options: ResultsOptions,
        /// The searchoptions for elasticlunr.js
        search_options: SearchOptions,
        /// Used to lookup a document's URL from an integer document ref.
        doc_urls: &'a Vec<String>,
        /// The index for elasticlunr.js
        index: &'a elasticlunr::Index,
    }

    let mut fields = BTreeMap::new();
    let mut opt = SearchOptionsField::default();
    let mut insert_boost = |key: &str, boost| {
        opt.boost = Some(boost);
        fields.insert(key.into(), opt);
    };
    insert_boost("title", search_config.boost_title);
    insert_boost("body", search_config.boost_paragraph);
    insert_boost("breadcrumbs", search_config.boost_hierarchy);

    let search_options = SearchOptions {
        bool: if search_config.use_boolean_and {
            SearchBool::And
        } else {
            SearchBool::Or
        },
        expand: search_config.expand,
        fields,
    };

    let results_options = ResultsOptions {
        limit_results: search_config.limit_results,
        teaser_word_count: search_config.teaser_word_count,
    };

    let json_contents = SearchindexJson {
        results_options,
        search_options,
        doc_urls,
        index,
    };

    // By converting to serde_json::Value as an intermediary, we use a
    // BTreeMap internally and can force a stable ordering of map keys.
    let json_contents =
        serde_json::to_value(&json_contents).context("Failed to serialize search index to JSON")?;
    let json_contents = serde_json::to_string(&json_contents)
        .context("Failed to serialize search index to JSON string")?;

    Ok(json_contents)
}
