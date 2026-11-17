//! Card blocks: fenced Markdown blocks that a card draws as native Teams elements, so a message
//! can show the attributes of an item, links as buttons, status badges, progress, a chart,
//! collapsible detail or a colored callout. A block that does not parse stays code, and every
//! element that a client may not draw carries a text fallback, so nothing written is lost. The
//! syntax is plain Markdown to anything else (the GUI draws the same blocks, `markdown.js`).
//!
//! Only what Teams resolves by itself: links that open (HTTPS, checked like every Markdown
//! link) and sections that show or hide. No input or submit action: the message is the user's,
//! and no bot receives one.
use super::{LINK, Layout, elements, inline, text_block};
use crate::adapters::teams;
use crate::config::Language;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};

/// What a model or an agent is told about the blocks: the prompts of answers, summaries and
/// shortening, and the descriptions of the tools that post messages or save their templates.
pub const GUIDE: &str = crate::card_guide!();
/// [`GUIDE`] as a literal, for the tool descriptions that `concat!` builds.
#[macro_export]
macro_rules! card_guide {
    () => {
        "Card blocks: Teams messages are cards, and besides Markdown a message may hold these fenced blocks, drawn as native card elements. Use one only where it fits the content and makes it clearer, never as decoration; a short message stays a short paragraph. ```facts with one `Name: value` per line: the attributes of one item (state, owner, sprint, branch, version, environment, dates). ```buttons with one `[Label](https://…)` per line, optionally followed by ` — short description` (then they show as large buttons): what to open (the work item, a pull request, a pipeline run, a page). ```badges with one `color: text` per line, color green (done, available), orange (pending, partial), red (failed, blocked), blue (in progress) or gray (not applicable): a few short status labels. ```progress with one `Label: done/total` or `Label: NN%` per line: progress bars (stages, tasks, a sprint). ```chart KIND Title, KIND being donut, pie, bar, hbar, line or gauge, with one `label: number` per line (gauge: one `value/max` line): numbers worth comparing (hours per task, items per state, a weekly trend). ```details Title with Markdown inside: hidden behind a Title toggle, for evidence, logs or long detail after a short answer (a block inside it names its kind or language after the backticks). A quote whose first line is [!NOTE], [!TIP], [!IMPORTANT], [!WARNING] or [!CAUTION] is a colored callout. Inline {green:…}, {orange:…}, {red:…}, {blue:…} and {gray:…} color a word or two of status. Combine them when it helps: facts and badges for one item, progress then buttons for a run, a chart then details."
    };
}

/// Primary buttons in a row; the rest go to its «…» menu.
const PRIMARY_BUTTONS: usize = 4;
/// Badges side by side; more start another row, so a phone never scrolls sideways.
const BADGES_PER_ROW: usize = 3;
/// IDs of the hidden parts of `details` blocks: unique within any card.
static DETAILS: AtomicUsize = AtomicUsize::new(0);

/// The element of the fenced block whose info string is `info` and whose lines are `lines`, or
/// `None` when it is not a card block (or has nothing a block can show): then it is code.
pub(super) fn fenced(
    info: &str,
    lines: &[String],
    layout: Layout,
    language: Language,
) -> Option<Value> {
    let (kind, rest) = info.split_once(char::is_whitespace).unwrap_or((info, ""));
    let rest = rest.trim();
    match kind.to_lowercase().as_str() {
        "facts" => facts(lines),
        "buttons" => buttons(lines),
        "badges" => badges(lines),
        "progress" => progress(lines),
        "chart" => chart(rest, lines),
        "details" => details(rest, lines, layout, language),
        _ => None,
    }
}

/// A block's line without its list mark: `- a` and `a` are the same entry.
fn entry(line: &str) -> &str {
    let line = line.trim();
    ["- ", "* ", "+ "]
        .iter()
        .find_map(|mark| line.strip_prefix(mark))
        .unwrap_or(line)
        .trim()
}

/// `Name: value`, split at the first `: ` (or a final `:`); a line without one is all value.
fn pair(line: &str) -> (&str, &str) {
    match line.split_once(": ") {
        Some((name, value)) => (name.trim(), value.trim()),
        None => match line.strip_suffix(':') {
            Some(name) => (name.trim(), ""),
            None => ("", line),
        },
    }
}

/// Text without Markdown marks, for what a card shows as plain text (button titles, badges).
fn plain(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if c == '['
            && let Some((label, _, used)) = teams::link(rest)
        {
            out.push_str(label);
            rest = &rest[used..];
            continue;
        }
        if c == '{'
            && let Some((_, body, used)) = teams::colored(rest)
        {
            out.push_str(body);
            rest = &rest[used..];
            continue;
        }
        if !matches!(c, '*' | '`' | '~') && !(c == '_' && rest.starts_with("__")) {
            out.push(c);
        }
        rest = &rest[c.len_utf8()..];
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Several elements as one.
fn group(mut items: Vec<Value>) -> Option<Value> {
    match items.len() {
        0 => None,
        1 => items.pop(),
        _ => Some(json!({"type": "Container", "items": items})),
    }
}

fn facts(lines: &[String]) -> Option<Value> {
    let facts: Vec<Value> = lines
        .iter()
        .map(|l| entry(l))
        .filter(|l| !l.is_empty())
        .map(|l| {
            let (name, value) = pair(l);
            json!({"title": inline(name), "value": inline(value)})
        })
        .collect();
    (!facts.is_empty()).then(|| json!({"type": "FactSet", "facts": facts}))
}

fn buttons(lines: &[String]) -> Option<Value> {
    let mut links = Vec::new();
    let mut other = Vec::new();
    for line in lines.iter().map(|l| entry(l)).filter(|l| !l.is_empty()) {
        match line.starts_with('[').then(|| teams::link(line)).flatten() {
            Some((label, url, used)) => {
                let description = line[used..]
                    .trim()
                    .trim_start_matches(['—', '–', '-', ':'])
                    .trim();
                links.push((label, url, description));
            }
            None => other.push(text_block(inline(line))),
        }
    }
    if links.is_empty() {
        return None;
    }
    let mut items = other;
    if links
        .iter()
        .any(|(_, _, description)| !description.is_empty())
    {
        // Large buttons, one per line, each with its description.
        for (label, url, description) in &links {
            let title = plain(label);
            let fallback = format!("[{label}]({url}) {description}");
            items.push(json!({"type": "CompoundButton", "title": title,
                "description": plain(description), "icon": {"name": "Open"},
                "selectAction": {"type": "Action.OpenUrl", "title": title, "url": url},
                "fallback": text_block(inline(fallback.trim()))}));
        }
    } else {
        let actions: Vec<Value> = links
            .iter()
            .enumerate()
            .map(|(at, (label, url, _))| {
                let mut action =
                    json!({"type": "Action.OpenUrl", "title": plain(label), "url": url});
                if at >= PRIMARY_BUTTONS {
                    action["mode"] = json!("secondary");
                }
                action
            })
            .collect();
        items.push(json!({"type": "ActionSet", "actions": actions}));
    }
    group(items)
}

/// The color names of `{color:…}` spans and badges: the card's color, the badge's style, its
/// icon and an emoji for clients without badges.
const TONES: [(&str, &str, &str, &str, &str); 5] = [
    ("green", "Good", "Good", "CheckmarkCircle", "✅"),
    ("orange", "Warning", "Warning", "Clock", "🟠"),
    ("red", "Attention", "Attention", "ErrorCircle", "❌"),
    ("blue", "Accent", "Informative", "ArrowSync", "🔵"),
    ("gray", "Default", "Subtle", "", "⚪"),
];

fn badges(lines: &[String]) -> Option<Value> {
    let badges: Vec<Value> = lines
        .iter()
        .map(|l| entry(l))
        .filter(|l| !l.is_empty())
        .map(|line| {
            let (name, text) = pair(line);
            let tone = TONES
                .iter()
                .find(|t| t.0 == name.to_lowercase())
                .filter(|_| !text.is_empty());
            let (text, (_, color, style, icon, emoji)) = match tone {
                Some(tone) => (plain(text), *tone),
                None => (plain(line), TONES[4]),
            };
            let mut badge = json!({"type": "Badge", "text": text, "style": style,
                "appearance": "Tint", "fallback": {"type": "TextBlock",
                "text": format!("{emoji} {text}"), "color": color, "wrap": true}});
            if !icon.is_empty() {
                badge["icon"] = json!(icon);
            }
            badge
        })
        .collect();
    let rows: Vec<Value> = badges
        .chunks(BADGES_PER_ROW)
        .map(|row| {
            let columns: Vec<Value> = row
                .iter()
                .map(|b| json!({"type": "Column", "width": "auto", "items": [b]}))
                .collect();
            json!({"type": "ColumnSet", "spacing": "Small", "columns": columns})
        })
        .collect();
    group(rows)
}

/// A number as people write it: `7`, `7.5`, `7,5`, `65%`, `6h`.
fn number(text: &str) -> Option<f64> {
    let text = text
        .trim()
        .trim_end_matches(['%', 'h', 'H'])
        .trim()
        .replace(' ', "");
    let text = if text.contains('.') {
        text.replace(',', "")
    } else {
        text.replace(',', ".")
    };
    text.parse::<f64>().ok().filter(|n| n.is_finite())
}

/// `done/total` or `NN%`: what is done and of how much.
fn ratio(text: &str) -> Option<(f64, f64)> {
    let text = text.trim();
    let (done, total) = match text.split_once('/') {
        Some((done, total)) => (number(done)?, number(total)?),
        None if text.ends_with('%') => (number(text)?, 100.0),
        None => return None,
    };
    (total > 0.0 && done >= 0.0).then_some((done.min(total), total))
}

/// A number for a card: whole numbers without a decimal point.
fn value(n: f64) -> Value {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        json!(n as i64)
    } else {
        json!((n * 100.0).round() / 100.0)
    }
}

fn progress(lines: &[String]) -> Option<Value> {
    let mut items = Vec::new();
    let mut bars = 0;
    for line in lines.iter().map(|l| entry(l)).filter(|l| !l.is_empty()) {
        let (name, amount) = pair(line);
        let Some((done, total)) =
            ratio(amount).or_else(|| ratio(name).filter(|_| amount.is_empty()))
        else {
            items.push(text_block(inline(line)));
            continue;
        };
        bars += 1;
        let label = if amount.is_empty() {
            inline(name)
        } else {
            format!("{} · {}", inline(name), inline(amount))
        };
        let filled = ((done / total) * 10.0).round() as usize;
        let drawn = format!("{}{}", "▰".repeat(filled), "▱".repeat(10 - filled.min(10)));
        let color = if done >= total { "Good" } else { "Accent" };
        items.push(json!({"type": "TextBlock", "text": label, "wrap": true}));
        items.push(
            json!({"type": "ProgressBar", "value": value(done), "max": value(total),
            "color": color, "spacing": "Small",
            "fallback": {"type": "TextBlock", "text": drawn, "spacing": "None"}}),
        );
    }
    (bars > 0).then(|| group(items)).flatten()
}

fn chart(info: &str, lines: &[String]) -> Option<Value> {
    let (kind, title) = info.split_once(char::is_whitespace).unwrap_or((info, ""));
    let title = plain(title);
    let entries: Vec<(&str, &str)> = lines
        .iter()
        .map(|l| entry(l))
        .filter(|l| !l.is_empty())
        .map(pair)
        .collect();
    // Clients without charts read the numbers.
    let mut shown: Vec<Value> = Vec::new();
    if !title.is_empty() {
        shown.push(json!({"type": "TextBlock", "text": title, "weight": "Bolder", "wrap": true}));
    }
    shown.extend(
        entries
            .iter()
            .map(|(name, amount)| text_block(inline(&format!("{name}: {amount}")))),
    );
    let fallback = json!({"type": "Container", "items": shown});
    let mut chart = match kind.to_lowercase().as_str() {
        "gauge" => {
            let (done, total) = entries
                .iter()
                .find_map(|(name, amount)| ratio(amount).or_else(|| ratio(name)))?;
            let third = total / 3.0;
            json!({"type": "Chart.Gauge", "value": value(done), "min": 0, "max": value(total),
            "segments": [
                {"legend": format!("0–{}", value(third)), "size": value(third), "color": "attention"},
                {"legend": format!("{}–{}", value(third), value(2.0 * third)), "size": value(third), "color": "warning"},
                {"legend": format!("{}–{}", value(2.0 * third), value(total)), "size": value(total - 2.0 * third), "color": "good"},
            ]})
        }
        kind @ ("donut" | "pie" | "bar" | "hbar" | "line") => {
            let points: Vec<(String, f64)> = entries
                .iter()
                .filter_map(|(name, amount)| Some((plain(name), number(amount)?)))
                .filter(|(name, _)| !name.is_empty())
                .collect();
            if points.is_empty() {
                return None;
            }
            match kind {
                "donut" | "pie" => {
                    let data: Vec<Value> = points
                        .iter()
                        .map(|(x, y)| json!({"legend": x, "value": value(*y)}))
                        .collect();
                    let kind = if kind == "pie" {
                        "Chart.Pie"
                    } else {
                        "Chart.Donut"
                    };
                    json!({"type": kind, "data": data})
                }
                "line" => {
                    let values: Vec<Value> = points
                        .iter()
                        .map(|(x, y)| json!({"x": x, "y": value(*y)}))
                        .collect();
                    json!({"type": "Chart.Line", "data": [{"legend": title, "values": values}]})
                }
                _ => {
                    let data: Vec<Value> = points
                        .iter()
                        .map(|(x, y)| json!({"x": x, "y": value(*y)}))
                        .collect();
                    let kind = if kind == "hbar" {
                        "Chart.HorizontalBar"
                    } else {
                        "Chart.VerticalBar"
                    };
                    json!({"type": kind, "data": data})
                }
            }
        }
        _ => return None,
    };
    if !title.is_empty() {
        chart["title"] = json!(title);
    }
    chart["fallback"] = fallback;
    Some(chart)
}

fn details(title: &str, lines: &[String], layout: Layout, language: Language) -> Option<Value> {
    let items = elements(&lines.join("\n"), true, layout, language);
    if items.is_empty() {
        return None;
    }
    let title = match plain(title) {
        title if title.is_empty() => language.say("Details", "Detalles"),
        title => title,
    };
    let id = format!("pta-details-{}", DETAILS.fetch_add(1, Ordering::Relaxed));
    Some(json!({"type": "Container", "items": [
        {"type": "ActionSet", "actions": [{"type": "Action.ToggleVisibility",
            "title": format!("{title} ▾"), "targetElements": [id]}]},
        {"type": "Container", "id": id, "isVisible": false, "spacing": "Small", "items": items},
    ]}))
}

/// The container style and emoji of a quote whose first line is a callout mark (`[!NOTE]`…).
pub(super) fn callout(first: &str) -> Option<(&'static str, &'static str)> {
    let mark = first.trim().strip_prefix("[!")?.strip_suffix(']')?;
    Some(match mark.to_uppercase().as_str() {
        "NOTE" => ("emphasis", "ℹ️"),
        "TIP" => ("good", "💡"),
        "IMPORTANT" => ("accent", "❗"),
        "WARNING" => ("warning", "⚠️"),
        "CAUTION" => ("attention", "⛔"),
        _ => return None,
    })
}

/// Whether the card's body holds something that needs room: a table or a chart.
pub(super) fn wide(body: &Value) -> bool {
    match body {
        Value::Object(object) => {
            object
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|t| t == "Table" || t.starts_with("Chart."))
                || object
                    .iter()
                    .any(|(key, child)| key != "fallback" && wide(child))
        }
        Value::Array(items) => items.iter().any(wide),
        _ => false,
    }
}

/// The text a block's element shows, when it is not in `text` keys: facts, buttons, charts and
/// the runs of rich text (one piece, as written). `None` for every other element.
pub(super) fn read(element: &Value) -> Option<Vec<String>> {
    let kind = element["type"].as_str()?;
    let text = |v: &Value| v.as_str().unwrap_or("").to_owned();
    match kind {
        "RichTextBlock" if element.get("fallback").is_none() => {
            let line: String = element["inlines"]
                .as_array()?
                .iter()
                .map(|run| run["text"].as_str().unwrap_or(""))
                .collect();
            Some(vec![line])
        }
        "FactSet" => Some(
            element["facts"]
                .as_array()?
                .iter()
                .map(|f| format!("{}: {}", text(&f["title"]), text(&f["value"])))
                .collect(),
        ),
        "Action.OpenUrl" => Some(vec![text(&element["title"])]),
        // The toggle is presentation; what it shows is read where it is.
        "Action.ToggleVisibility" => Some(Vec::new()),
        "CompoundButton" => Some(vec![format!(
            "{} {}",
            text(&element["title"]),
            text(&element["description"])
        )]),
        kind if kind.starts_with("Chart.") => {
            let mut out = vec![text(&element["title"])];
            if kind == "Chart.Gauge" {
                out.push(format!("{}/{}", element["value"], element["max"]));
            }
            let points = element["data"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|d| {
                    d["values"]
                        .as_array()
                        .map_or_else(|| vec![d.clone()], |values| values.clone())
                });
            out.extend(points.map(|p| {
                let name = p.get("legend").or(p.get("x")).map(text).unwrap_or_default();
                let amount = p
                    .get("value")
                    .or(p.get("y"))
                    .cloned()
                    .unwrap_or(Value::Null);
                format!("{name}: {amount}")
            }));
            out.retain(|t| !t.is_empty());
            Some(out)
        }
        _ => None,
    }
}

/// The style of a run of rich text.
#[derive(Clone, Default)]
struct Style {
    bold: bool,
    italic: bool,
    strike: bool,
    code: bool,
    color: Option<&'static str>,
    subtle: bool,
    url: Option<String>,
}

/// A line with `{color:…}` spans as rich text (a text block cannot color words), after
/// `prefix`. `None` when the line has none, or has a mention: Teams resolves mentions in text
/// blocks only.
pub(super) fn rich(prefix: &str, text: &str) -> Option<Value> {
    let colored = text
        .match_indices('{')
        .any(|(at, _)| teams::colored(&text[at..]).is_some());
    if !colored || text.contains(super::MENTION_START) {
        return None;
    }
    let mut inlines = Vec::new();
    if !prefix.is_empty() {
        inlines.push(json!({"type": "TextRun", "text": prefix}));
    }
    runs(&text.replace(LINK, ""), &Style::default(), &mut inlines);
    Some(json!({"type": "RichTextBlock", "inlines": inlines}))
}

fn run(text: &str, style: &Style) -> Value {
    let mut run = json!({"type": "TextRun", "text": text});
    if style.bold {
        run["weight"] = json!("Bolder");
    }
    if style.italic {
        run["italic"] = json!(true);
    }
    if style.strike {
        run["strikethrough"] = json!(true);
    }
    if style.code {
        run["fontType"] = json!("Monospace");
    }
    if let Some(color) = style.color {
        run["color"] = json!(color);
    }
    if style.subtle {
        run["isSubtle"] = json!(true);
    }
    if let Some(url) = &style.url {
        run["selectAction"] = json!({"type": "Action.OpenUrl", "url": url});
        run["underline"] = json!(true);
        if style.color.is_none() {
            run["color"] = json!("Accent");
        }
    }
    run
}

/// The runs of `text` in `style`: bold, italic, strikethrough, code, colors and HTTPS links,
/// as `teams::html` reads them.
fn runs(text: &str, style: &Style, out: &mut Vec<Value>) {
    let mut plain = String::new();
    let flush = |plain: &mut String, out: &mut Vec<Value>| {
        if !plain.is_empty() {
            out.push(run(plain, style));
            plain.clear();
        }
    };
    let mut rest = text;
    let mut before: Option<char> = None;
    while let Some(c) = rest.chars().next() {
        let used = match c {
            '\\' => rest[1..]
                .chars()
                .next()
                .filter(char::is_ascii_punctuation)
                .map(|p| {
                    plain.push(p);
                    1 + p.len_utf8()
                }),
            '`' => rest[1..].find('`').map(|end| {
                flush(&mut plain, out);
                let code = Style {
                    code: true,
                    ..style.clone()
                };
                out.push(run(&rest[1..1 + end], &code));
                end + 2
            }),
            '*' | '_' | '~' => emphasis(rest, before).map(|(mark, inner)| {
                flush(&mut plain, out);
                let mut inside = style.clone();
                match &rest[..mark] {
                    "***" => (inside.bold, inside.italic) = (true, true),
                    "**" | "__" => inside.bold = true,
                    "~~" => inside.strike = true,
                    _ => inside.italic = true,
                }
                runs(&rest[mark..mark + inner], &inside, out);
                inner + 2 * mark
            }),
            '{' => teams::colored(rest).map(|(hex, body, used)| {
                flush(&mut plain, out);
                let mut inside = style.clone();
                let name = teams::COLORS
                    .iter()
                    .find(|(_, h)| *h == hex)
                    .map(|(n, _)| *n);
                match TONES.iter().find(|t| Some(t.0) == name) {
                    Some(tone) if tone.0 == "gray" => inside.subtle = true,
                    Some(tone) => inside.color = Some(tone.1),
                    None => {}
                }
                runs(body, &inside, out);
                used
            }),
            '[' => teams::link(rest).map(|(label, url, used)| {
                flush(&mut plain, out);
                let inside = Style {
                    url: Some(url.to_owned()),
                    ..style.clone()
                };
                runs(label, &inside, out);
                used
            }),
            _ => None,
        };
        let used = used.unwrap_or_else(|| {
            plain.push(c);
            c.len_utf8()
        });
        before = rest[..used].chars().next_back();
        rest = &rest[used..];
    }
    flush(&mut plain, out);
}

/// The emphasis at the start of `text` (`***`, `**`, `__`, `~~`, `*` or `_`): the length of its
/// mark and of the text inside. The same rules as `teams::html`: the text inside neither starts
/// nor ends with a space, and underscores count only at word boundaries.
fn emphasis(text: &str, before: Option<char>) -> Option<(usize, usize)> {
    let word = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
    for mark in ["***", "**", "__", "~~", "*", "_"] {
        let underscore = mark.starts_with('_');
        let Some(inner) = text.strip_prefix(mark) else {
            continue;
        };
        if inner.starts_with(char::is_whitespace) || (underscore && word(before)) {
            continue;
        }
        let mut from = 0;
        while let Some(at) = inner[from..].find(mark).map(|at| at + from) {
            let run = inner[at..]
                .bytes()
                .take_while(|b| *b == mark.as_bytes()[0])
                .count();
            let closes = at > 0
                && !inner[..at].ends_with(char::is_whitespace)
                && !(underscore && word(inner[at + mark.len()..].chars().next()))
                && (mark.len() > 1 || run == 1);
            if closes {
                return Some((mark.len(), at));
            }
            from = at + if mark.len() == 1 { run } else { 1 };
        }
    }
    None
}
