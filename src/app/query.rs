use super::{App, Catalog, Outcome, Record, Stage, StageFilter, Warning};
use crate::config::VaultConfig;
use crate::error::{Error, Result};
use crate::index::{Cancellation, FreshnessMode, Index, IndexedLink};
use crate::note::{Diagnostic, Severity};
use nucleo_matcher::{
    Config, Matcher, Utf32Str,
    pattern::{AtomKind, CaseMatching, Normalization, Pattern},
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SearchMode {
    #[default]
    Default,
    Fts,
    Literal,
    Regex,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LinkDirection {
    Incoming,
    Outgoing,
    #[default]
    Both,
}

impl App {
    pub fn with_freshness(mut self, mode: FreshnessMode) -> Self {
        self.freshness_mode = mode;
        self
    }
    fn query_index(&self, token: &Cancellation) -> Result<(Index, Catalog)> {
        token.check()?;
        if self.freshness_mode == FreshnessMode::NotApplicable {
            return Err(Error::new("usage", "Выберите full, stat или cached"));
        }
        let mut index = if self.freshness_mode == FreshnessMode::Cached {
            Index::open_cached(&self.vault)?
        } else {
            Index::open(&self.vault)?
        };
        let config = VaultConfig::load(&self.vault)?;
        let catalog = index.reconcile(&self.vault, &config, self.freshness_mode, token)?;
        Ok((index, catalog))
    }
    pub fn catalog(&self, token: &Cancellation) -> Result<Catalog> {
        self.query_index(token).map(|(_, catalog)| catalog)
    }
    pub fn preview(&self, path: &str, token: &Cancellation) -> Result<Record> {
        token.check()?;
        let raw = String::from_utf8(self.vault.read(path)?)
            .map_err(|_| Error::new("validation_failed", "Файл не UTF-8"))?;
        token.check()?;
        Ok(Record::from_text(path.into(), raw))
    }
    pub fn show(&self, id: &str) -> Result<Outcome> {
        self.show_cancelled(id, &Cancellation::default())
    }
    pub fn show_cancelled(&self, id: &str, token: &Cancellation) -> Result<Outcome> {
        let (mut index, observed) = self.query_index(token)?;
        let (mut catalog, body) = index
            .note_snapshot(id, token)
            .map_err(|error| observed_error(error, &observed))?;
        match_observation(&mut catalog, &observed);
        let record = catalog.find(id)?;
        let mut data = record.data();
        data["body"] = json!(body);
        data["metadata"] = json!(record.note.fields);
        data["links"] = json!(record.note.links);
        let mut out = catalog_outcome(&catalog, data);
        out.complete = record.readable;
        out.diagnostics = record.note.diagnostics.clone();
        Ok(out)
    }
    pub fn list(
        &self,
        stage: StageFilter,
        tag: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Outcome> {
        self.list_cancelled(stage, tag, limit, offset, &Cancellation::default())
    }
    pub fn list_cancelled(
        &self,
        stage: StageFilter,
        tag: Option<&str>,
        limit: usize,
        offset: usize,
        token: &Cancellation,
    ) -> Result<Outcome> {
        check_page(limit, offset)?;
        let catalog = self.catalog(token)?;
        let mut items = Vec::new();
        let mut count = 0usize;
        for record in &catalog.records {
            token.check()?;
            if stage_matches(stage, record.stage)
                && tag.is_none_or(|tag| record.tags.iter().any(|t| t == tag))
            {
                if count >= offset && items.len() <= limit {
                    items.push(record.data());
                }
                count += 1;
            }
        }
        Ok(page(&catalog, items, limit, offset))
    }
    pub fn search(
        &self,
        query: &str,
        mode: SearchMode,
        limit: usize,
        offset: usize,
        token: &Cancellation,
    ) -> Result<Outcome> {
        check_page(limit, offset)?;
        check_query(query)?;
        let regex = if mode == SearchMode::Regex {
            Some(
                regex::RegexBuilder::new(query)
                    .size_limit(1024 * 1024)
                    .dfa_size_limit(1024 * 1024)
                    .build()
                    .map_err(|error| {
                        Error::new("usage", format!("Неверное регулярное выражение: {error}"))
                    })?,
            )
        } else {
            None
        };
        let (mut index, observed) = self.query_index(token)?;
        if matches!(mode, SearchMode::Default | SearchMode::Fts) {
            let (mut catalog, hits) = index
                .search_fts_snapshot(query, mode == SearchMode::Fts, limit + 1, offset, token)
                .map_err(|error| observed_error(error, &observed))?;
            match_observation(&mut catalog, &observed);
            let records: BTreeMap<_, _> = catalog
                .records
                .iter()
                .map(|r| (r.path.as_str(), r))
                .collect();
            let items = hits
                .into_iter()
                .filter_map(|hit| {
                    records.get(hit.path.as_str()).map(|record| {
                        let mut data = record.data();
                        data["score"] = json!(hit.score);
                        data
                    })
                })
                .collect();
            return Ok(page(&catalog, items, limit, offset));
        }
        let mut catalog = observed;
        if self.freshness_mode == FreshnessMode::Cached {
            catalog.warnings.push(Warning { code: "live_content".into(), message: "Список путей взят из cached; буквальный/regex поиск читает их текущее содержимое без сверки".into(), path: None });
        }
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut items = Vec::new();
        let mut count = 0usize;
        let mut read_diagnostics = Vec::new();
        if !query.is_empty() {
            for record in &catalog.records {
                token.check()?;
                if Instant::now() >= deadline {
                    return Err(Error::new(
                        "interrupted",
                        "Поиск превысил ограничение 15 секунд",
                    ));
                }
                let raw = match self.vault.read(&record.path).and_then(|bytes| {
                    String::from_utf8(bytes)
                        .map_err(|_| Error::new("validation_failed", "Файл не UTF-8"))
                }) {
                    Ok(raw) => raw,
                    Err(error) => {
                        catalog.complete = false;
                        let mut diagnostic = Diagnostic::new(
                            "zetto/file-read-error",
                            Severity::Error,
                            error.message,
                        );
                        diagnostic.path = Some(record.path.clone());
                        read_diagnostics.push(diagnostic);
                        continue;
                    }
                };
                let found = regex.as_ref().map_or_else(
                    || raw.find(query).map(|start| (start, start + query.len())),
                    |re| re.find(&raw).map(|m| (m.start(), m.end())),
                );
                if let Some((start, end)) = found {
                    if count >= offset && items.len() <= limit {
                        let mut data = record.data();
                        data["score"] = json!(1);
                        data["match_span"] = json!({"start":start,"end":end});
                        items.push(data);
                    }
                    count += 1;
                }
            }
        }
        let mut out = page(&catalog, items, limit, offset);
        out.diagnostics.extend(read_diagnostics);
        Ok(out)
    }
    fn graph_snapshot(&self, token: &Cancellation) -> Result<(Catalog, Vec<IndexedLink>)> {
        let (mut index, observed) = self.query_index(token)?;
        let (mut catalog, links) = index
            .links_snapshot(token)
            .map_err(|error| observed_error(error, &observed))?;
        match_observation(&mut catalog, &observed);
        Ok((catalog, links))
    }
    pub fn links(
        &self,
        id: &str,
        direction: LinkDirection,
        token: &Cancellation,
    ) -> Result<Outcome> {
        let (catalog, links) = self.graph_snapshot(token)?;
        let record = catalog.find(id)?;
        let records: BTreeMap<_, _> = catalog
            .records
            .iter()
            .map(|r| (r.path.as_str(), r))
            .collect();
        let mut items = Vec::new();
        for link in links {
            token.check()?;
            if (direction != LinkDirection::Incoming && link.source_path == record.path)
                || (direction != LinkDirection::Outgoing
                    && link.target_path.as_deref() == Some(record.path.as_str()))
            {
                items.push(link_data(&link, &records));
            }
        }
        Ok(catalog_outcome(&catalog, json!({"id":id,"items":items})))
    }
    pub fn graph(&self, id: &str, depth: usize, token: &Cancellation) -> Result<Outcome> {
        if !(1..=3).contains(&depth) {
            return Err(Error::new("usage", "depth должен быть от 1 до 3"));
        }
        let (catalog, links) = self.graph_snapshot(token)?;
        let start = catalog.find(id)?.path.clone();
        let records: BTreeMap<_, _> = catalog
            .records
            .iter()
            .map(|r| (r.path.as_str(), r))
            .collect();
        let mut adjacency: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for (i, link) in links.iter().enumerate() {
            token.check()?;
            adjacency.entry(&link.source_path).or_default().push(i);
            if let Some(target) = &link.target_path {
                adjacency.entry(target).or_default().push(i);
            }
        }
        let mut queue = VecDeque::from([(start.clone(), 0usize)]);
        let mut visited = BTreeSet::from([start]);
        let mut edges = BTreeSet::new();
        let mut truncated = false;
        while let Some((path, distance)) = queue.pop_front() {
            token.check()?;
            if distance == depth {
                continue;
            }
            for i in adjacency.get(path.as_str()).into_iter().flatten() {
                if edges.len() >= 20_000 && !edges.contains(i) {
                    truncated = true;
                    continue;
                }
                let link = &links[*i];
                // Неразрешённые ссылки остаются видимыми рёбрами без выдуманного узла.
                let neighbor = if link.source_path == path {
                    link.target_path.as_deref()
                } else {
                    Some(link.source_path.as_str())
                };
                if let Some(neighbor) = neighbor
                    && !visited.contains(neighbor)
                {
                    if visited.len() >= 10_000 {
                        truncated = true;
                        continue;
                    }
                    visited.insert(neighbor.to_owned());
                    queue.push_back((neighbor.to_owned(), distance + 1));
                }
                edges.insert(*i);
            }
        }
        let nodes: Vec<_> = catalog
            .records
            .iter()
            .filter(|r| visited.contains(&r.path))
            .map(Record::data)
            .collect();
        let edges: Vec<_> = edges
            .into_iter()
            .map(|i| link_data(&links[i], &records))
            .collect();
        let mut out = catalog_outcome(
            &catalog,
            json!({"id":id,"depth":depth,"nodes":nodes,"edges":edges}),
        );
        out.truncated = truncated;
        out.complete &= !truncated;
        Ok(out)
    }
    pub fn tags(&self, token: &Cancellation) -> Result<Outcome> {
        let catalog = self.catalog(token)?;
        let mut counts = BTreeMap::<&str, usize>::new();
        for record in &catalog.records {
            token.check()?;
            for tag in &record.tags {
                *counts.entry(tag).or_default() += 1;
            }
        }
        let items: Vec<_> = counts
            .into_iter()
            .map(|(tag, count)| json!({"tag":tag,"count":count}))
            .collect();
        Ok(catalog_outcome(&catalog, json!({"items":items})))
    }
    pub fn paths(&self, ids: &[String], token: &Cancellation) -> Result<Outcome> {
        if ids.is_empty() {
            return Err(Error::new("usage", "Укажите хотя бы один ULID"));
        }
        let catalog = self.catalog(token)?;
        let mut records: Vec<_> = ids
            .iter()
            .map(|id| catalog.find(id))
            .collect::<Result<_>>()?;
        records.sort_by(|a, b| {
            a.note
                .id()
                .cmp(&b.note.id())
                .then_with(|| a.path.cmp(&b.path))
        });
        let items: Vec<_> = records
            .into_iter()
            .map(
                |record| json!({"id":record.note.id(),"path":self.vault.root().join(&record.path)}),
            )
            .collect();
        Ok(catalog_outcome(&catalog, json!({"items":items})))
    }
    pub fn rebuild_index(&self, token: &Cancellation) -> Result<Outcome> {
        let mut index = Index::open(&self.vault)?;
        let catalog = index.rebuild(&self.vault, &VaultConfig::load(&self.vault)?, token)?;
        Ok(catalog_outcome(
            &catalog,
            json!({"generation":catalog.freshness.generation,"files":catalog.records.len(),"metrics":catalog.metrics}),
        ))
    }
    pub fn fuzzy_titles(
        &self,
        query: &str,
        stage: StageFilter,
        limit: usize,
        token: &Cancellation,
    ) -> Result<Outcome> {
        check_page(limit, 0)?;
        check_query(query)?;
        let catalog = self.catalog(token)?;
        let pattern = Pattern::new(
            query,
            CaseMatching::Ignore,
            Normalization::Smart,
            AtomKind::Fuzzy,
        );
        let mut matcher = Matcher::new(Config::DEFAULT);
        let mut buffer = Vec::new();
        let mut matching = Vec::new();
        for (index, record) in catalog.records.iter().enumerate() {
            token.check()?;
            if record.id().is_none() || !stage_matches(stage, record.stage) {
                continue;
            }
            let score = std::iter::once(record.note.title().unwrap_or("").to_owned())
                .chain(record.note.aliases())
                .filter_map(|text| pattern.score(Utf32Str::new(&text, &mut buffer), &mut matcher))
                .max();
            if let Some(score) = score {
                matching.push((index, score));
            }
        }
        matching.sort_by(|(a, sa), (b, sb)| sb.cmp(sa).then_with(|| a.cmp(b)));
        let items = matching
            .into_iter()
            .take(limit.min(50) + 1)
            .map(|(i, score)| {
                let mut data = catalog.records[i].data();
                data["score"] = json!(score);
                data
            })
            .collect();
        Ok(page(&catalog, items, limit.min(50), 0))
    }
}

fn check_page(limit: usize, offset: usize) -> Result<()> {
    if !(1..=10_000).contains(&limit) || i64::try_from(offset).is_err() {
        Err(Error::new("usage", "Неверный limit/offset"))
    } else {
        Ok(())
    }
}
fn check_query(query: &str) -> Result<()> {
    if query.len() > 64 * 1024 {
        Err(Error::new("usage", "Запрос превышает 64 KiB"))
    } else {
        Ok(())
    }
}
fn stage_matches(filter: StageFilter, stage: Stage) -> bool {
    matches!(
        (filter, stage),
        (StageFilter::All, _)
            | (StageFilter::Published, Stage::Published)
            | (StageFilter::Inbox, Stage::Inbox)
    )
}
fn catalog_outcome(catalog: &Catalog, data: Value) -> Outcome {
    let mut out = Outcome::ok(data);
    out.freshness = catalog.freshness.clone();
    out.warnings = catalog.warnings.clone();
    out.complete = catalog.complete;
    out.diagnostics = catalog
        .records
        .iter()
        .flat_map(|r| r.note.diagnostics.clone())
        .collect();
    out
}
fn page(catalog: &Catalog, mut items: Vec<Value>, limit: usize, offset: usize) -> Outcome {
    let truncated = items.len() > limit;
    items.truncate(limit);
    let mut out = catalog_outcome(
        catalog,
        json!({"items":items,"next_offset":truncated.then_some(offset.saturating_add(limit))}),
    );
    out.truncated = truncated;
    out.complete &= !truncated;
    out
}
fn match_observation(snapshot: &mut Catalog, observed: &Catalog) {
    if snapshot.freshness.generation == observed.freshness.generation
        && !snapshot.warnings.iter().any(|w| w.code == "index_stale")
    {
        snapshot.freshness = observed.freshness.clone();
        snapshot.metrics = observed.metrics.clone();
    } else if observed.freshness.mode != FreshnessMode::Cached {
        snapshot.warnings.push(Warning {
            code: "index_stale".into(),
            message: "Индекс изменился после сверки; показано согласованное cached поколение"
                .into(),
            path: None,
        });
    }
}
fn link_data(link: &IndexedLink, records: &BTreeMap<&str, &Record>) -> Value {
    json!({"source_id":records.get(link.source_path.as_str()).and_then(|r| r.note.id()),"source_path":link.source_path,"target":link.link.target,"kind":link.link.kind,"display":link.link.display,"span":link.link.span,"resolved_id":link.target_path.as_deref().and_then(|p| records.get(p)).and_then(|r| r.id()),"resolved_path":link.target_path,"reason":link.reason})
}

fn observed_error(mut error: Error, observed: &Catalog) -> Error {
    error.context.freshness = observed.freshness.clone();
    error.context.warnings.extend(observed.warnings.clone());
    error
}
