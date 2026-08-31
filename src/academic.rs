use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::path::Path;

use quick_xml::Reader;
use quick_xml::events::Event;
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zip::ZipArchive;

use crate::outline::{OutlineEntry, build_outline};
use crate::{Error, read_source};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcademicContract {
    pub version: u32,
    pub central_claim: String,
    pub explanandum: String,
    pub allow_formal_research_question: bool,
    pub reject_defensive_caveats: bool,
    pub required_order: Vec<String>,
    pub terms: Vec<TermContract>,
    pub proper_nouns: Vec<String>,
    /// 段落第一文が進めるべき論旨を、本文に出る順で記録する。
    pub first_sentence_order: Vec<String>,
    pub section_bridges: Vec<SectionBridge>,
    pub note_decisions: Vec<NoteDecision>,
    pub style_profile: StyleProfile,
    #[serde(default)]
    pub sync_omit_prefixes: Vec<String>,
    #[serde(default = "default_mutable_entries")]
    pub docx_mutable_entries: Vec<String>,
}

fn default_mutable_entries() -> Vec<String> {
    vec![
        "word/document.xml".to_owned(),
        "docProps/core.xml".to_owned(),
        "docProps/app.xml".to_owned(),
    ]
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TermContract {
    pub term: String,
    pub status: String,
    pub source: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SectionBridge {
    pub from: String,
    pub to: String,
    pub shared_terms: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoteDecision {
    pub marker: String,
    pub disposition: String,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StyleProfile {
    pub sentence_length_mean_min: f64,
    pub sentence_length_mean_max: f64,
    pub citation_led_paragraph_ratio_max: f64,
    #[serde(default)]
    pub connectives: Vec<String>,
    #[serde(default)]
    pub discovery_marker_at_paragraph_end: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportRecord {
    pub exporter: String,
    pub pdf_visual_reviewed: bool,
    pub source_sha256: String,
    pub docx_sha256: String,
    pub pdf_sha256: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct AcademicCheck {
    pub id: String,
    pub status: String,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct StyleObservations {
    pub sentence_length_mean: f64,
    pub paragraph_count: usize,
    pub citation_led_paragraph_ratio: f64,
    pub connective_counts: BTreeMap<String, usize>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AcademicReport {
    pub passed: bool,
    pub delivery_ready: bool,
    pub checks: Vec<AcademicCheck>,
    pub first_sentences: Vec<OutlineEntry>,
    pub style: StyleObservations,
}

pub struct ArtifactPaths<'a> {
    pub source: &'a Path,
    pub docx: Option<&'a Path>,
    pub pdf: Option<&'a Path>,
    pub template: Option<&'a Path>,
    pub export_record: Option<&'a Path>,
}

fn add_check(checks: &mut Vec<AcademicCheck>, id: &str, passed: bool, detail: impl Into<String>) {
    checks.push(AcademicCheck {
        id: id.to_owned(),
        status: if passed { "pass" } else { "fail" }.to_owned(),
        detail: detail.into(),
    });
}

fn add_review(checks: &mut Vec<AcademicCheck>, id: &str, detail: impl Into<String>) {
    checks.push(AcademicCheck {
        id: id.to_owned(),
        status: "review".to_owned(),
        detail: detail.into(),
    });
}

fn validate_contract(contract: &AcademicContract) -> Result<(), Error> {
    if contract.version != 1 {
        return Err(Error::Academic(format!(
            "contract version {} は未対応です",
            contract.version
        )));
    }
    if contract.central_claim.trim().is_empty() || contract.explanandum.trim().is_empty() {
        return Err(Error::Academic(
            "central_claim と explanandum は空にできません".to_owned(),
        ));
    }
    if contract.required_order.is_empty() || contract.first_sentence_order.is_empty() {
        return Err(Error::Academic(
            "required_order と first_sentence_order は少なくとも1項目必要です".to_owned(),
        ));
    }
    if contract.terms.is_empty() {
        return Err(Error::Academic(
            "terms は空にできません。普通名詞も ordinary として明示してください".to_owned(),
        ));
    }
    for term in &contract.terms {
        if !matches!(
            term.status.as_str(),
            "established" | "source-specific" | "coined" | "ordinary"
        ) {
            return Err(Error::Academic(format!(
                "未知の用語statusです: {}",
                term.status
            )));
        }
        if matches!(term.status.as_str(), "established" | "source-specific")
            && term.source.as_deref().is_none_or(str::is_empty)
        {
            return Err(Error::Academic(format!(
                "{} にはsourceが必要です",
                term.term
            )));
        }
    }
    for note in &contract.note_decisions {
        if !matches!(note.disposition.as_str(), "body" | "note" | "drop") {
            return Err(Error::Academic(format!(
                "{} のnote dispositionが不正です",
                note.marker
            )));
        }
    }
    Ok(())
}

fn normalized(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_whitespace())
        .filter(|ch| !matches!(ch, '*' | '_' | '`'))
        .collect()
}

fn visible_paragraphs(source: &str, omit_prefixes: &[String]) -> Vec<String> {
    let mut paragraphs = Vec::new();
    let mut buffer = Vec::new();
    let mut front_matter = false;
    let mut fence: Option<char> = None;
    let flush = |buffer: &mut Vec<String>, paragraphs: &mut Vec<String>| {
        let text = buffer.join(" ").trim().to_owned();
        buffer.clear();
        let trimmed = text.trim_start_matches('#').trim();
        if trimmed.chars().count() >= 8
            && !trimmed.starts_with('|')
            && !trimmed.starts_with("![")
            && !trimmed.starts_with("[^")
            && !omit_prefixes
                .iter()
                .any(|prefix| trimmed.starts_with(prefix))
        {
            paragraphs.push(normalized(trimmed));
        }
    };
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        if index == 0 && trimmed == "---" {
            front_matter = true;
            continue;
        }
        if front_matter {
            if trimmed == "---" {
                front_matter = false;
            }
            continue;
        }
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            flush(&mut buffer, &mut paragraphs);
            let marker = trimmed.chars().next().unwrap_or('`');
            if fence == Some(marker) {
                fence = None;
            } else if fence.is_none() {
                fence = Some(marker);
            }
            continue;
        }
        if fence.is_some() {
            continue;
        }
        if trimmed.is_empty() {
            flush(&mut buffer, &mut paragraphs);
        } else {
            buffer.push(trimmed.to_owned());
        }
    }
    flush(&mut buffer, &mut paragraphs);
    paragraphs
}

fn prose_paragraphs(source: &str) -> Vec<String> {
    source
        .split("\n\n")
        .map(str::trim)
        .filter(|part| !part.is_empty() && !part.starts_with('#') && !part.starts_with('|'))
        .map(str::to_owned)
        .collect()
}

fn sentences(text: &str) -> Vec<String> {
    text.split_inclusive(['。', '！', '？'])
        .map(str::trim)
        .filter(|sentence| !sentence.is_empty())
        .map(str::to_owned)
        .collect()
}

fn style_observations(source: &str, profile: &StyleProfile) -> StyleObservations {
    let paragraphs = prose_paragraphs(source);
    let sentence_list = sentences(source);
    let mean = if sentence_list.is_empty() {
        0.0
    } else {
        sentence_list
            .iter()
            .map(|sentence| sentence.chars().count() as f64)
            .sum::<f64>()
            / sentence_list.len() as f64
    };
    let citation_lead = Regex::new(r"^[A-Za-z一-龠々・]{2,24}[（(][12][0-9]{3}[）)]")
        .expect("valid citation-lead regex");
    let citation_led = paragraphs
        .iter()
        .filter(|paragraph| citation_lead.is_match(paragraph))
        .count();
    let ratio = if paragraphs.is_empty() {
        0.0
    } else {
        citation_led as f64 / paragraphs.len() as f64
    };
    let connective_counts = profile
        .connectives
        .iter()
        .map(|word| (word.clone(), source.matches(word).count()))
        .collect();
    StyleObservations {
        sentence_length_mean: (mean * 100.0).round() / 100.0,
        paragraph_count: paragraphs.len(),
        citation_led_paragraph_ratio: (ratio * 1000.0).round() / 1000.0,
        connective_counts,
    }
}

fn section_bounds(outline: &[OutlineEntry], heading: &str) -> Option<(usize, usize)> {
    let start = outline
        .iter()
        .position(|entry| entry.kind == "heading" && entry.text == heading)?;
    let level = outline[start].level.unwrap_or(1);
    let end = outline[start + 1..]
        .iter()
        .position(|entry| entry.kind == "heading" && entry.level.unwrap_or(1) <= level)
        .map_or(outline.len(), |offset| start + 1 + offset);
    Some((start, end))
}

fn academic_outline(source: &str) -> Vec<OutlineEntry> {
    let mut in_references = false;
    build_outline(source)
        .into_iter()
        .filter(|entry| {
            if entry.kind == "heading"
                && matches!(
                    entry.text.as_str(),
                    "参考文献" | "引用文献" | "References" | "Bibliography"
                )
            {
                in_references = true;
            }
            !in_references && !(entry.kind == "lead" && entry.text.starts_with("[^"))
        })
        .collect()
}

fn audit_bridges(
    checks: &mut Vec<AcademicCheck>,
    outline: &[OutlineEntry],
    rules: &[SectionBridge],
) {
    for rule in rules {
        let Some((from_start, from_end)) = section_bounds(outline, &rule.from) else {
            add_check(
                checks,
                "section_bridge",
                false,
                format!("見出しがありません: {}", rule.from),
            );
            continue;
        };
        let Some((to_start, to_end)) = section_bounds(outline, &rule.to) else {
            add_check(
                checks,
                "section_bridge",
                false,
                format!("見出しがありません: {}", rule.to),
            );
            continue;
        };
        let from = outline[from_start..from_end]
            .iter()
            .rev()
            .find(|entry| entry.kind == "lead")
            .map(|entry| entry.text.as_str())
            .unwrap_or_default();
        let to = outline[to_start..to_end]
            .iter()
            .find(|entry| entry.kind == "lead")
            .map(|entry| entry.text.as_str())
            .unwrap_or_default();
        let shared = rule
            .shared_terms
            .iter()
            .any(|term| from.contains(term) && to.contains(term));
        add_check(
            checks,
            "section_bridge",
            shared,
            format!(
                "{} -> {}: 前節末「{}」 / 次節冒頭「{}」",
                rule.from, rule.to, from, to
            ),
        );
    }
}

fn audit_first_sentence_order(
    checks: &mut Vec<AcademicCheck>,
    outline: &[OutlineEntry],
    required: &[String],
) {
    let leads = outline
        .iter()
        .filter(|entry| entry.kind == "lead")
        .map(|entry| entry.text.as_str())
        .collect::<Vec<_>>();
    let mut next = 0;
    let mut missing = None;
    for required_text in required {
        let Some(offset) = leads[next..]
            .iter()
            .position(|lead| lead.contains(required_text))
        else {
            missing = Some(required_text);
            break;
        };
        next += offset + 1;
    }
    add_check(
        checks,
        "first_sentence_order",
        missing.is_none(),
        missing.map_or_else(
            || "段落第一文の論旨順を確認".to_owned(),
            |text| format!("段落第一文にない、又は順序が崩れた命題: {text}"),
        ),
    );
}

fn audit_proper_nouns(checks: &mut Vec<AcademicCheck>, source: &str, known: &[String]) {
    let citation = Regex::new(r"([A-Za-z一-龠々・]{2,30})[（(][12][0-9]{3}[a-z]?[）)]")
        .expect("valid citation regex");
    let reference_start = source
        .find("参考文献")
        .or_else(|| source.find("References"));
    let body = reference_start.map_or(source, |start| &source[..start]);
    let unknown = citation
        .captures_iter(body)
        .map(|capture| capture[1].to_owned())
        .filter(|name| !known.iter().any(|known_name| known_name == name))
        .collect::<BTreeSet<_>>();
    if !unknown.is_empty() {
        add_review(
            checks,
            "unregistered_proper_noun",
            format!(
                "契約にない本文引用の固有名詞候補: {}",
                unknown.into_iter().collect::<Vec<_>>().join(", ")
            ),
        );
    }
}

fn audit_citations(checks: &mut Vec<AcademicCheck>, source: &str) {
    let citation = Regex::new(r"([A-Za-z一-龠々・]{2,30})[（(]([12][0-9]{3})[a-z]?[）)]")
        .expect("valid citation regex");
    let reference_start = source
        .find("参考文献")
        .or_else(|| source.find("References"));
    let body = reference_start.map_or(source, |start| &source[..start]);
    let references = reference_start.map_or("", |start| &source[start..]);
    let entries = references
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect::<Vec<_>>();
    let mut missing = BTreeSet::new();
    for capture in citation.captures_iter(body) {
        let author = capture.get(1).map_or("", |item| item.as_str());
        let year = capture.get(2).map_or("", |item| item.as_str());
        if !entries
            .iter()
            .any(|entry| entry.contains(author) && entry.contains(year))
        {
            missing.insert(format!("{author}（{year}）"));
        }
    }
    add_check(
        checks,
        "citation_reference_match",
        missing.is_empty(),
        if missing.is_empty() {
            "本文引用に対応する著者名・年が参考文献欄にあります".to_owned()
        } else {
            format!(
                "参考文献欄で対応を確認できません: {}",
                missing.into_iter().collect::<Vec<_>>().join(", ")
            )
        },
    );
}

fn audit_notes(checks: &mut Vec<AcademicCheck>, source: &str, decisions: &[NoteDecision]) {
    let note = Regex::new(r"(?m)^\[\^([^]]+)\]:\s*(.+)$").expect("valid note regex");
    let definitions = note
        .captures_iter(source)
        .map(|capture| (capture[1].to_owned(), capture[2].to_owned()))
        .collect::<BTreeMap<_, _>>();
    let body = note.replace_all(source, "");
    for decision in decisions {
        let marker = decision
            .marker
            .trim_start_matches("[^")
            .trim_end_matches(']');
        let passed = match decision.disposition.as_str() {
            "note" => definitions
                .get(marker)
                .is_some_and(|text| text.contains(&decision.text)),
            "body" => {
                body.contains(&decision.text)
                    && !definitions
                        .values()
                        .any(|text| text.contains(&decision.text))
            }
            "drop" => !source.contains(&decision.text),
            _ => false,
        };
        add_check(
            checks,
            "note_disposition",
            passed,
            format!("{} -> {}", decision.marker, decision.disposition),
        );
    }
    let decided = decisions
        .iter()
        .map(|item| {
            item.marker
                .trim_start_matches("[^")
                .trim_end_matches(']')
                .to_owned()
        })
        .collect::<BTreeSet<_>>();
    let undecided = definitions
        .keys()
        .filter(|marker| !decided.contains(*marker))
        .cloned()
        .collect::<Vec<_>>();
    add_check(
        checks,
        "note_coverage",
        undecided.is_empty(),
        if undecided.is_empty() {
            "全注に本文・注・不要の判定があります".to_owned()
        } else {
            format!("判定のない注: {}", undecided.join(", "))
        },
    );
}

fn zip_entries(path: &Path) -> Result<BTreeMap<String, Vec<u8>>, Error> {
    let file = File::open(path)
        .map_err(|error| Error::Academic(format!("{}を開けません: {error}", path.display())))?;
    let mut archive = ZipArchive::new(file).map_err(|error| {
        Error::Academic(format!(
            "{}は有効なDOCXではありません: {error}",
            path.display()
        ))
    })?;
    let mut entries = BTreeMap::new();
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| Error::Academic(error.to_string()))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_owned();
        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .map_err(|error| Error::Academic(error.to_string()))?;
        entries.insert(name, bytes);
    }
    Ok(entries)
}

fn docx_text(path: &Path) -> Result<String, Error> {
    let entries = zip_entries(path)?;
    let xml = entries
        .get("word/document.xml")
        .ok_or_else(|| Error::Academic("DOCXにword/document.xmlがありません".to_owned()))?;
    let mut reader = Reader::from_reader(xml.as_slice());
    let mut output = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Text(text)) => {
                let decoded = text
                    .decode()
                    .map_err(|error| Error::Academic(error.to_string()))?;
                let unescaped = quick_xml::escape::unescape(&decoded)
                    .map_err(|error| Error::Academic(error.to_string()))?;
                output.push_str(&unescaped);
            }
            Ok(Event::End(end)) if end.name().as_ref() == b"w:p" => output.push('\n'),
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => return Err(Error::Academic(error.to_string())),
        }
    }
    Ok(output)
}

fn mutable(name: &str, rules: &[String]) -> bool {
    rules.iter().any(|rule| {
        rule.strip_suffix('*')
            .map_or(name == rule, |prefix| name.starts_with(prefix))
    })
}

fn audit_ooxml(
    checks: &mut Vec<AcademicCheck>,
    template: &Path,
    docx: &Path,
    mutable_entries: &[String],
) -> Result<(), Error> {
    let before = zip_entries(template)?;
    let after = zip_entries(docx)?;
    let changed = before
        .keys()
        .chain(after.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|name| !mutable(name, mutable_entries) && before.get(*name) != after.get(*name))
        .cloned()
        .collect::<Vec<_>>();
    add_check(
        checks,
        "ooxml_invariants",
        changed.is_empty(),
        if changed.is_empty() {
            "許可していないテンプレートOOXMLは不変です".to_owned()
        } else {
            format!("許可なく変わったOOXML: {}", changed.join(", "))
        },
    );
    Ok(())
}

fn file_sha256(path: &Path) -> Result<String, Error> {
    let bytes = std::fs::read(path).map_err(|source| Error::Read {
        path: path.display().to_string(),
        source,
    })?;
    Ok(Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn audit_export_record(
    checks: &mut Vec<AcademicCheck>,
    record_path: &Path,
    paths: &ArtifactPaths<'_>,
) -> Result<(), Error> {
    let record: ExportRecord = serde_json::from_str(&read_source(record_path)?)?;
    add_check(
        checks,
        "word_exporter",
        record.exporter == "Microsoft Word",
        format!("記録されたexporter: {}", record.exporter),
    );
    add_check(
        checks,
        "pdf_visual_review",
        record.pdf_visual_reviewed,
        "PDF全頁を画像として目視確認した記録を確認",
    );
    let source_ok = file_sha256(paths.source)? == record.source_sha256;
    let docx_ok = paths
        .docx
        .is_some_and(|path| file_sha256(path).is_ok_and(|hash| hash == record.docx_sha256));
    let pdf_ok = paths
        .pdf
        .is_some_and(|path| file_sha256(path).is_ok_and(|hash| hash == record.pdf_sha256));
    add_check(
        checks,
        "export_record_hashes",
        source_ok && docx_ok && pdf_ok,
        "納品記録のMarkdown・DOCX・PDFハッシュを照合しました",
    );
    Ok(())
}

fn audit_sync(
    checks: &mut Vec<AcademicCheck>,
    source: &str,
    contract: &AcademicContract,
    paths: &ArtifactPaths<'_>,
) -> Result<(), Error> {
    let paragraphs = visible_paragraphs(source, &contract.sync_omit_prefixes);
    if let Some(docx) = paths.docx {
        let text = normalized(&docx_text(docx)?);
        let missing = paragraphs
            .iter()
            .filter(|paragraph| !text.contains(paragraph.as_str()))
            .count();
        add_check(
            checks,
            "markdown_docx_sync",
            missing == 0,
            format!(
                "{}段落中、DOCXで確認できない段落: {}",
                paragraphs.len(),
                missing
            ),
        );
    }
    if let Some(pdf) = paths.pdf {
        let extracted = pdf_extract::extract_text(pdf)
            .map_err(|error| Error::Academic(format!("PDF本文を抽出できません: {error}")))?;
        let text = normalized(&extracted);
        let missing = paragraphs
            .iter()
            .filter(|paragraph| !text.contains(paragraph.as_str()))
            .count();
        add_check(
            checks,
            "markdown_pdf_sync",
            missing == 0,
            format!(
                "{}段落中、PDFで確認できない段落: {}",
                paragraphs.len(),
                missing
            ),
        );
    }
    Ok(())
}

pub fn audit(
    source: &str,
    contract: &AcademicContract,
    paths: &ArtifactPaths<'_>,
) -> Result<AcademicReport, Error> {
    validate_contract(contract)?;
    let mut checks = Vec::new();
    add_check(
        &mut checks,
        "central_claim",
        source.contains(&contract.central_claim),
        "中心命題を本文で確認",
    );
    add_check(
        &mut checks,
        "explanandum",
        source.contains(&contract.explanandum),
        "説明対象を本文で確認",
    );

    let rq =
        Regex::new(r"(?i)リサーチ[・ ]?クエスチョン|\bRQ\s*[0-9０-９]").expect("valid RQ regex");
    add_check(
        &mut checks,
        "research_question_policy",
        contract.allow_formal_research_question || !rq.is_match(source),
        "形式的なリサーチクエスチョンの扱いを確認",
    );

    let mut last = 0;
    let mut order_ok = true;
    for item in &contract.required_order {
        if let Some(offset) = source[last..].find(item) {
            last += offset + item.len();
        } else {
            order_ok = false;
            break;
        }
    }
    add_check(
        &mut checks,
        "required_order",
        order_ok,
        format!("固定順序: {}", contract.required_order.join(" -> ")),
    );

    for term in &contract.terms {
        let found = source.find(&term.term);
        let passed = found.is_some_and(|offset| {
            let start = source[..offset]
                .char_indices()
                .rev()
                .nth(80)
                .map_or(0, |(index, _)| index);
            let term_end = offset + term.term.len();
            let end = source[term_end..]
                .char_indices()
                .nth(80)
                .map_or(source.len(), |(index, _)| term_end + index);
            let context = &source[start..end];
            match term.status.as_str() {
                "coined" => context.contains("造語"),
                "established" | "source-specific" => term
                    .source
                    .as_deref()
                    .is_some_and(|name| context.contains(name)),
                "ordinary" => true,
                _ => false,
            }
        });
        add_check(
            &mut checks,
            "term_provenance",
            passed,
            format!("{} ({})", term.term, term.status),
        );
    }

    let registered = contract
        .terms
        .iter()
        .map(|item| item.term.as_str())
        .chain(contract.proper_nouns.iter().map(String::as_str))
        .collect::<BTreeSet<_>>();
    let quoted = Regex::new(r"[「《]([^」》]{2,24})[」》]").expect("valid label regex");
    let unregistered = quoted
        .captures_iter(source)
        .map(|capture| capture[1].to_owned())
        .filter(|term| !registered.contains(term.as_str()))
        .collect::<BTreeSet<_>>();
    if !unregistered.is_empty() {
        add_review(
            &mut checks,
            "unregistered_label",
            format!(
                "契約にないラベル候補: {}",
                unregistered.into_iter().collect::<Vec<_>>().join(", ")
            ),
        );
    }

    let caveat = Regex::new(
        r"必ずしも.{0,30}ない|可能性がある|対象に含めない|直接示すものではない|わけではない",
    )
    .expect("valid caveat regex");
    let caveats = caveat
        .find_iter(source)
        .map(|item| item.as_str())
        .collect::<Vec<_>>();
    if contract.reject_defensive_caveats {
        add_check(
            &mut checks,
            "defensive_caveats",
            caveats.is_empty(),
            if caveats.is_empty() {
                "防御的留保テンプレートなし".to_owned()
            } else {
                format!("候補: {}", caveats.join(", "))
            },
        );
    } else if !caveats.is_empty() {
        add_review(
            &mut checks,
            "defensive_caveats",
            format!("候補: {}", caveats.join(", ")),
        );
    }

    let outline = academic_outline(source);
    let first_sentences = outline
        .iter()
        .filter(|entry| matches!(entry.kind.as_str(), "heading" | "lead"))
        .cloned()
        .collect::<Vec<_>>();
    let section_count = outline
        .iter()
        .filter(|entry| entry.kind == "heading")
        .count();
    add_check(
        &mut checks,
        "section_bridge_contract",
        section_count < 2 || !contract.section_bridges.is_empty(),
        if section_count < 2 {
            "節が一つのため節間接続の契約は不要です".to_owned()
        } else if contract.section_bridges.is_empty() {
            "複数節の原稿にはsection_bridgesを記録します".to_owned()
        } else {
            "節間接続の契約を確認".to_owned()
        },
    );
    audit_bridges(&mut checks, &outline, &contract.section_bridges);
    audit_first_sentence_order(&mut checks, &outline, &contract.first_sentence_order);
    audit_citations(&mut checks, source);
    audit_proper_nouns(&mut checks, source, &contract.proper_nouns);
    audit_notes(&mut checks, source, &contract.note_decisions);

    let profile = &contract.style_profile;
    let style = style_observations(source, profile);
    add_check(
        &mut checks,
        "style_sentence_length",
        style.sentence_length_mean >= profile.sentence_length_mean_min
            && style.sentence_length_mean <= profile.sentence_length_mean_max,
        format!("平均文長: {}", style.sentence_length_mean),
    );
    add_check(
        &mut checks,
        "style_citation_leads",
        style.citation_led_paragraph_ratio <= profile.citation_led_paragraph_ratio_max,
        format!(
            "著者名・年で始まる段落率: {}",
            style.citation_led_paragraph_ratio
        ),
    );
    if profile.discovery_marker_at_paragraph_end {
        let misplaced = prose_paragraphs(source)
            .iter()
            .filter(|paragraph| {
                paragraph.contains("とわかる") && !paragraph.trim_end().ends_with("とわかる。")
            })
            .count();
        add_check(
            &mut checks,
            "style_discovery_marker",
            misplaced == 0,
            format!("「とわかる」が段落末以外にある段落: {misplaced}"),
        );
    }

    let delivery_paths_complete = matches!(
        (paths.docx, paths.pdf, paths.template, paths.export_record),
        (Some(_), Some(_), Some(_), Some(_))
    );
    let any_delivery_path = [paths.docx, paths.pdf, paths.template, paths.export_record]
        .iter()
        .any(Option::is_some);
    if any_delivery_path && !delivery_paths_complete {
        add_check(
            &mut checks,
            "delivery_artifacts",
            false,
            "提出監査には --docx、--pdf、--template、--export-record をすべて指定します",
        );
    } else if !delivery_paths_complete {
        add_review(
            &mut checks,
            "delivery_readiness",
            "原稿監査のみ完了。提出前にはWord DOCX、PDF、テンプレート、目視確認済み納品記録を指定します",
        );
    }
    audit_sync(&mut checks, source, contract, paths)?;
    if let (Some(template), Some(docx)) = (paths.template, paths.docx) {
        audit_ooxml(&mut checks, template, docx, &contract.docx_mutable_entries)?;
    }
    if let Some(record) = paths.export_record {
        audit_export_record(&mut checks, record, paths)?;
    }
    let passed = checks.iter().all(|item| item.status != "fail");
    let delivery_ready = passed
        && delivery_paths_complete
        && checks
            .iter()
            .any(|item| item.id == "pdf_visual_review" && item.status == "pass");
    Ok(AcademicReport {
        passed,
        delivery_ready,
        checks,
        first_sentences,
        style,
    })
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::Write;

    use tempfile::tempdir;
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    use super::{AcademicContract, ArtifactPaths, audit, file_sha256};

    fn write_docx(path: &std::path::Path, text: &str, style: &str, extra: Option<(&str, &str)>) {
        let file = File::create(path).expect("create docx");
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        zip.start_file("[Content_Types].xml", options)
            .expect("content types");
        zip.write_all(b"<Types/>").expect("write content types");
        zip.start_file("word/styles.xml", options).expect("styles");
        zip.write_all(style.as_bytes()).expect("write styles");
        zip.start_file("word/document.xml", options)
            .expect("document");
        zip.write_all(
            format!(
                r#"<w:document xmlns:w="urn:test"><w:body><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:body></w:document>"#
            )
            .as_bytes(),
        )
        .expect("write document");
        if let Some((name, contents)) = extra {
            zip.start_file(name, options).expect("extra entry");
            zip.write_all(contents.as_bytes())
                .expect("write extra entry");
        }
        zip.finish().expect("finish docx");
    }

    fn write_pdf(path: &std::path::Path, text: &str) {
        let content = format!("BT /F1 12 Tf 72 720 Td ({text}) Tj ET");
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_owned(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
            format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        ];
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (index, object) in objects.iter().enumerate() {
            offsets.push(bytes.len());
            bytes.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
        }
        let xref = bytes.len();
        bytes.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for offset in offsets {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        std::fs::write(path, bytes).expect("write PDF");
    }

    fn write_export_record(
        path: &std::path::Path,
        source: &std::path::Path,
        docx: &std::path::Path,
        pdf: &std::path::Path,
        pdf_visual_reviewed: bool,
    ) {
        let record = format!(
            r#"{{"exporter":"Microsoft Word","pdf_visual_reviewed":{pdf_visual_reviewed},"source_sha256":"{}","docx_sha256":"{}","pdf_sha256":"{}"}}"#,
            file_sha256(source).expect("source hash"),
            file_sha256(docx).expect("docx hash"),
            file_sha256(pdf).expect("pdf hash"),
        );
        std::fs::write(path, record).expect("write export record");
    }

    fn contract() -> AcademicContract {
        serde_json::from_str(
            r#"{
                "version": 1,
                "central_claim": "Policy body has evidence.",
                "explanandum": "Policy",
                "allow_formal_research_question": false,
                "reject_defensive_caveats": true,
                "required_order": ["Policy"],
                "terms": [{"term": "Policy", "status": "ordinary"}],
                "proper_nouns": [],
                "first_sentence_order": ["Policy body has evidence."],
                "section_bridges": [],
                "note_decisions": [],
                "style_profile": {
                    "sentence_length_mean_min": 1.0,
                    "sentence_length_mean_max": 100.0,
                    "citation_led_paragraph_ratio_max": 1.0
                }
            }"#,
        )
        .expect("contract")
    }

    #[test]
    fn artifact_audit_checks_sync_and_template_invariants() {
        let dir = tempdir().expect("tempdir");
        let source = dir.path().join("paper.md");
        let template = dir.path().join("template.docx");
        let docx = dir.path().join("paper.docx");
        let pdf = dir.path().join("paper.pdf");
        let record = dir.path().join("delivery-record.json");
        std::fs::write(&source, "# Introduction\n\nPolicy body has evidence.\n").expect("source");
        write_docx(&template, "雛形", "<styles>fixed</styles>", None);
        write_docx(
            &docx,
            "Introduction Policy body has evidence.",
            "<styles>fixed</styles>",
            None,
        );
        write_pdf(&pdf, "Introduction Policy body has evidence.");
        write_export_record(&record, &source, &docx, &pdf, true);

        let report = audit(
            &std::fs::read_to_string(&source).expect("read source"),
            &contract(),
            &ArtifactPaths {
                source: &source,
                docx: Some(&docx),
                pdf: Some(&pdf),
                template: Some(&template),
                export_record: Some(&record),
            },
        )
        .expect("audit");
        assert!(report.passed, "{report:#?}");
        assert!(report.delivery_ready);

        write_export_record(&record, &source, &docx, &pdf, false);
        let unreviewed = audit(
            &std::fs::read_to_string(&source).expect("read source"),
            &contract(),
            &ArtifactPaths {
                source: &source,
                docx: Some(&docx),
                pdf: Some(&pdf),
                template: Some(&template),
                export_record: Some(&record),
            },
        )
        .expect("audit");
        assert!(!unreviewed.delivery_ready);
        assert!(
            unreviewed
                .checks
                .iter()
                .any(|item| item.id == "pdf_visual_review" && item.status == "fail")
        );

        let partial = audit(
            &std::fs::read_to_string(&source).expect("read source"),
            &contract(),
            &ArtifactPaths {
                source: &source,
                docx: Some(&docx),
                pdf: None,
                template: None,
                export_record: None,
            },
        )
        .expect("audit");
        assert!(!partial.passed);
        assert!(
            partial
                .checks
                .iter()
                .any(|item| item.id == "delivery_artifacts" && item.status == "fail")
        );

        write_docx(
            &docx,
            "Introduction Policy body has evidence.",
            "<styles>changed</styles>",
            None,
        );
        let changed = audit(
            &std::fs::read_to_string(&source).expect("read source"),
            &contract(),
            &ArtifactPaths {
                source: &source,
                docx: Some(&docx),
                pdf: Some(&pdf),
                template: Some(&template),
                export_record: None,
            },
        )
        .expect("audit");
        assert!(!changed.passed);
        assert!(
            changed
                .checks
                .iter()
                .any(|item| item.id == "ooxml_invariants" && item.status == "fail")
        );

        write_docx(
            &docx,
            "Introduction Policy body has evidence.",
            "<styles>fixed</styles>",
            Some(("word/header1.xml", "<header>new</header>")),
        );
        let added = audit(
            &std::fs::read_to_string(&source).expect("read source"),
            &contract(),
            &ArtifactPaths {
                source: &source,
                docx: Some(&docx),
                pdf: Some(&pdf),
                template: Some(&template),
                export_record: None,
            },
        )
        .expect("audit");
        assert!(
            added
                .checks
                .iter()
                .any(|item| item.id == "ooxml_invariants" && item.status == "fail")
        );
    }
}
