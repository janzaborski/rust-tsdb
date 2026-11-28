use crate::{
    Db,
    model::{LabelSet, Matcher, MatcherOperator, Sample, TimeRange},
};
use serde_json::{Value, json};

#[derive(Debug)]
pub enum Expr {
    Selector(String, Vec<(String, String)>),
    Scalar(f64),
    Window {
        function: WindowFunction,
        selector: Selector,
        duration: u64,
    },
}

#[derive(Debug)]
pub struct Selector {
    name: Option<String>,
    labels: Vec<(String, String)>,
}

impl Selector {
    fn named(name: String, labels: Vec<(String, String)>) -> Self {
        Self {
            name: Some(name),
            labels,
        }
    }

    pub(crate) fn index_matchers(&self) -> Vec<Matcher> {
        self.name
            .iter()
            .map(|name| Matcher::new("__name__", name.clone(), MatcherOperator::Equal))
            .chain(
                self.labels
                    .iter()
                    .filter(|(_, value)| !value.is_empty())
                    .map(|(key, value)| {
                        Matcher::new(key.clone(), value.clone(), MatcherOperator::Equal)
                    }),
            )
            .collect()
    }

    pub(crate) fn matches(&self, labels: &LabelSet) -> bool {
        self.name
            .as_deref()
            .is_none_or(|name| labels.get("__name__") == Some(name))
            && self
                .labels
                .iter()
                .all(|(key, value)| labels.get(key).unwrap_or("") == value)
    }
}

#[derive(Clone, Copy, Debug)]
pub enum WindowFunction {
    Avg,
    Min,
    Max,
    Sum,
    Count,
}

fn sum(values: impl Iterator<Item = f64>) -> f64 {
    let mut total: f64 = 0.0;
    let mut correction = 0.0;
    for value in values {
        let next = total + value;
        if next.is_finite() {
            correction += if total.abs() >= value.abs() {
                (total - next) + value
            } else {
                (value - next) + total
            };
        } else {
            correction = 0.0;
        }
        total = next;
    }
    total + correction
}

impl WindowFunction {
    fn evaluate(self, samples: &[Sample]) -> f64 {
        let values = || samples.iter().map(|sample| sample.value);
        let count = samples.len() as f64;
        match self {
            Self::Count => count,
            Self::Min => values().fold(f64::NAN, f64::min),
            Self::Max => values().fold(f64::NAN, f64::max),
            Self::Sum => sum(values()),
            Self::Avg => {
                let total = sum(values());
                if total.is_infinite() && values().all(f64::is_finite) {
                    // Scale before summation when the unscaled total overflows.
                    let scale = values().map(f64::abs).fold(0.0, f64::max);
                    (sum(values().map(|value| value / scale)) / count).clamp(-1.0, 1.0) * scale
                } else {
                    total / count
                }
            }
        }
    }
}

fn identifier(s: &str, metric: bool) -> bool {
    let mut c = s.chars();
    c.next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || (metric && c == ':'))
        && c.all(|c| c.is_ascii_alphanumeric() || c == '_' || (metric && c == ':'))
}

pub fn parse(input: &str) -> Result<Expr, String> {
    let s = input.trim();
    // Scalar constants and a single addition support datasource connection probes.
    if let Ok(v) = s.parse::<f64>()
        && v.is_finite()
    {
        return Ok(Expr::Scalar(v));
    }
    if let Some((a, b)) = s.split_once('+')
        && let (Ok(a), Ok(b)) = (a.trim().parse::<f64>(), b.trim().parse::<f64>())
        && (a + b).is_finite()
    {
        return Ok(Expr::Scalar(a + b));
    }
    let name_end = identifier_end(s, true);
    let tail = s[name_end..].trim_start();
    if let Some(argument) = tail.strip_prefix('(') {
        let function = match &s[..name_end] {
            "avg_over_time" => WindowFunction::Avg,
            "min_over_time" => WindowFunction::Min,
            "max_over_time" => WindowFunction::Max,
            "sum_over_time" => WindowFunction::Sum,
            "count_over_time" => WindowFunction::Count,
            _ => return Err("at byte 0: unsupported function".into()),
        };
        let (selector, rest) = parse_selector(s, argument.trim_start(), false)?;
        let err =
            |rest: &str, message: &str| format!("at byte {}: {message}", s.len() - rest.len());
        let window = rest
            .strip_prefix('[')
            .ok_or_else(|| err(rest, "expected window"))?;
        let (duration, rest) = window
            .split_once(']')
            .ok_or_else(|| err(window, "expected closing bracket"))?;
        let duration = window_duration(duration.trim()).map_err(|message| err(window, message))?;
        let rest = rest.trim_start();
        let rest = rest
            .strip_prefix(')')
            .ok_or_else(|| err(rest, "expected closing parenthesis"))?;
        finish_selector(s, rest.trim_start())?;
        return Ok(Expr::Window {
            function,
            selector,
            duration,
        });
    }
    let (selector, rest) = parse_selector(s, s, false)?;
    finish_selector(s, rest)?;
    Ok(Expr::Selector(s[..name_end].into(), selector.labels))
}

fn identifier_end(s: &str, metric: bool) -> usize {
    s.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || (metric && c == ':')))
        .unwrap_or(s.len())
}

fn finish_selector(input: &str, rest: &str) -> Result<(), String> {
    if rest.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "at byte {}: unexpected trailing text",
            input.len() - rest.len()
        ))
    }
}

pub(crate) fn parse_discovery_selector(input: &str) -> Result<Selector, String> {
    let input = input.trim();
    let (selector, rest) = parse_selector(input, input, true)?;
    finish_selector(input, rest)?;
    if selector.name.is_none() && selector.labels.iter().all(|(_, value)| value.is_empty()) {
        return Err("at byte 0: expected a metric name or a non-empty matching value".into());
    }
    Ok(selector)
}

fn parse_selector<'a>(
    s: &'a str,
    input: &'a str,
    allow_unnamed: bool,
) -> Result<(Selector, &'a str), String> {
    let n = identifier_end(input, true);
    let name = &input[..n];
    let unnamed = allow_unnamed && input.starts_with('{');
    if !unnamed && !identifier(name, true) {
        return Err(format!(
            "at byte {}: expected metric name",
            s.len() - input.len()
        ));
    }
    let mut rest = input[n..].trim_start();
    let name = (!unnamed).then(|| name.to_owned());
    let mut labels = Vec::new();
    if !rest.starts_with('{') {
        return Ok((Selector { name, labels }, rest));
    }
    rest = rest[1..].trim_start();
    while !rest.starts_with('}') {
        let pos = s.len() - rest.len();
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        let key = &rest[..end];
        if !identifier(key, false) {
            return Err(format!("at byte {pos}: expected label name"));
        }
        if labels.iter().any(|(k, _)| k == key) {
            return Err(format!("at byte {pos}: duplicate label"));
        }
        rest = rest[end..].trim_start();
        rest = rest
            .strip_prefix('=')
            .ok_or_else(|| format!("at byte {pos}: only = is supported"))?
            .trim_start();
        if !rest.starts_with('"') {
            return Err(format!("at byte {pos}: expected quoted value"));
        }
        let mut chars = rest.char_indices().skip(1);
        let mut value = String::new();
        let mut consumed = None;
        while let Some((i, c)) = chars.next() {
            match c {
                '"' => {
                    consumed = Some(i + 1);
                    break;
                }
                '\\' => value.push(match chars.next().map(|(_, c)| c) {
                    Some('n') => '\n',
                    Some('r') => '\r',
                    Some('t') => '\t',
                    Some('\\') => '\\',
                    Some('"') => '"',
                    _ => return Err(format!("at byte {pos}: unsupported escape")),
                }),
                c if c.is_control() => return Err(format!("at byte {pos}: control character")),
                c => value.push(c),
            }
        }
        rest =
            rest[consumed.ok_or_else(|| format!("at byte {pos}: unclosed string"))?..].trim_start();
        labels.push((key.to_owned(), value));
        if let Some(r) = rest.strip_prefix(',') {
            rest = r.trim_start();
        } else if !rest.starts_with('}') {
            return Err(format!(
                "at byte {}: expected comma or closing brace",
                s.len() - rest.len()
            ));
        }
    }
    Ok((Selector { name, labels }, rest[1..].trim_start()))
}

fn window_duration(mut input: &str) -> Result<u64, &'static str> {
    const UNITS: [(&str, u64); 7] = [
        ("ms", 1),
        ("s", 1_000),
        ("m", 60_000),
        ("h", 3_600_000),
        ("d", 86_400_000),
        ("w", 604_800_000),
        ("y", 31_536_000_000),
    ];
    let mut duration = 0_u64;
    let mut previous = u64::MAX;
    while !input.is_empty() {
        let n = input
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(input.len());
        let (number, unit) = input.split_at(n);
        let number = number
            .parse::<u64>()
            .map_err(|_| "invalid window duration")?;
        let (rest, scale) = UNITS
            .iter()
            .find_map(|(suffix, scale)| unit.strip_prefix(suffix).map(|rest| (rest, *scale)))
            .ok_or("expected window duration unit")?;
        if scale >= previous {
            return Err("window units must be unique and descending");
        }
        duration = number
            .checked_mul(scale)
            .and_then(|amount| duration.checked_add(amount))
            .ok_or("window duration out of range")?;
        input = rest;
        previous = scale;
    }
    if duration == 0 {
        return Err("window duration must be positive");
    }
    Ok(duration)
}

pub fn timestamp(s: &str) -> Result<u64, String> {
    if let Ok(t) = s.parse::<f64>() {
        return milliseconds(t);
    }
    let t = time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
        .map_err(|_| "expected Unix seconds or RFC3339")?;
    if t.unix_timestamp_nanos() < 0 {
        return Err("timestamp out of range".into());
    }
    u64::try_from(t.unix_timestamp_nanos() / 1_000_000).map_err(|_| "timestamp out of range".into())
}
fn milliseconds(t: f64) -> Result<u64, String> {
    let ms = t * 1000.0;
    if !ms.is_finite() || ms < 0.0 || ms >= u64::MAX as f64 {
        return Err("time out of range".into());
    }
    Ok(ms as u64)
}
pub fn step(s: &str) -> Result<u64, String> {
    let (s, scale) = if let Some(s) = s.strip_suffix("ms") {
        (s, 0.001)
    } else if let Some(s) = s.strip_suffix('s') {
        (s, 1.0)
    } else if let Some(s) = s.strip_suffix('m') {
        (s, 60.0)
    } else if let Some(s) = s.strip_suffix('h') {
        (s, 3600.0)
    } else {
        (s, 1.0)
    };
    let n = milliseconds(s.parse::<f64>().map_err(|_| "invalid step")? * scale)?;
    if n == 0 {
        return Err("step must be at least 1 ms".into());
    }
    Ok(n)
}

pub fn evaluate(
    db: &Db,
    expr: Expr,
    start: u64,
    end: u64,
    step: u64,
    instant: bool,
    lookback_ms: u64,
) -> Result<Value, String> {
    if start > end || step == 0 {
        return Err("invalid time range or step".into());
    }
    let count = ((end - start) / step)
        .checked_add(1)
        .ok_or("evaluation count overflow")?;
    if count > 11_000 {
        return Err("query exceeds 11000 evaluation times".into());
    }
    let times = || (0..count).map(|k| start + k * step);
    let mut result = Vec::new();
    match expr {
        Expr::Scalar(v) => {
            if instant {
                return Ok(
                    json!({"resultType":"scalar","result":[start as f64/1000.0,v.to_string()]}),
                );
            }
            result.push(json!({"metric":{},"values":times().map(|t| json!([t as f64/1000.0,v.to_string()])).collect::<Vec<_>>()}));
        }
        expr => {
            let (selector, window) = match expr {
                Expr::Selector(name, labels) => (Selector::named(name, labels), None),
                Expr::Window {
                    function,
                    selector,
                    duration,
                } => (selector, Some((function, duration))),
                Expr::Scalar(_) => unreachable!(),
            };
            let lookback = window.map_or(lookback_ms, |(_, duration)| duration);
            let series = db
                .query(
                    &selector.index_matchers(),
                    TimeRange::new(start.saturating_sub(lookback), end),
                )
                .map_err(|e| e.to_string())?;
            let mut points = 0;
            for s in series {
                if !selector.matches(&s.labels) {
                    continue;
                }
                let (mut first, mut end) = (0, 0);
                let mut previous = None;
                let mut values = Vec::new();
                for t in times() {
                    while end < s.samples.len() && s.samples[end].timestamp <= t {
                        end += 1;
                    }
                    let value = if let Some((function, duration)) = window {
                        if let Some(lower) = t.checked_sub(duration) {
                            while first < end && s.samples[first].timestamp <= lower {
                                first += 1;
                            }
                        }
                        if first == end {
                            continue;
                        }
                        let bounds = (first, end);
                        let value = match previous {
                            Some((old, value)) if old == bounds => value,
                            _ => function.evaluate(&s.samples[first..end]),
                        };
                        previous = Some((bounds, value));
                        match value {
                            f64::INFINITY => "+Inf".to_owned(),
                            f64::NEG_INFINITY => "-Inf".to_owned(),
                            _ => value.to_string(),
                        }
                    } else {
                        if end == 0 || s.samples[end - 1].timestamp < t.saturating_sub(lookback) {
                            continue;
                        }
                        s.samples[end - 1].value.to_string()
                    };
                    points += 1;
                    if points > 1_000_000 {
                        return Err("query exceeds 1000000 result points".into());
                    }
                    values.push(json!([t as f64 / 1000.0, value]));
                }
                if values.is_empty() {
                    continue;
                }
                let metric: serde_json::Map<String, Value> = s
                    .labels
                    .iter()
                    .filter(|(key, _)| window.is_none() || *key != "__name__")
                    .map(|(key, value)| (key.into(), json!(value)))
                    .collect();
                result.push(if instant {
                    json!({"metric":metric,"value":values[0]})
                } else {
                    json!({"metric":metric,"values":values})
                });
            }
        }
    }
    Ok(json!({"resultType":if instant {"vector"} else {"matrix"},"result":result}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        WriteBatch,
        model::{LabelSet, Sample},
    };
    fn evaluate(
        db: &Db,
        expr: Expr,
        start: u64,
        end: u64,
        step: u64,
        instant: bool,
    ) -> Result<Value, String> {
        super::evaluate(
            db,
            expr,
            start,
            end,
            step,
            instant,
            crate::DEFAULT_QUERY_LOOKBACK_MS,
        )
    }

    #[test]
    fn selectors_and_errors() {
        for s in ["cpu", "cpu{}", "cpu{ host = \"a\", room=\"x\\\"y\" }"] {
            assert!(parse(s).is_ok(), "{s}");
        }
        for s in [
            "",
            "cpu[5m]",
            "avg(cpu)",
            "cpu{host!=\"a\"}",
            "cpu{a=\"b\",a=\"c\"}",
            "cpu{a=\"x\"}junk",
            "cpu{",
        ] {
            assert!(parse(s).is_err(), "{s}");
        }
    }
    #[test]
    fn lookback_and_grid() {
        let db = Db::new();
        db.write(WriteBatch {
            series: vec![(
                LabelSet::from_pairs([("__name__", "cpu")]),
                vec![Sample::new(1000, 2.0), Sample::new(3000, 4.0)],
            )],
        })
        .unwrap();
        let r = evaluate(
            &db,
            parse("cpu{missing=\"\"}").unwrap(),
            2000,
            4000,
            1000,
            false,
        )
        .unwrap();
        assert_eq!(
            r["result"][0]["values"],
            json!([[2.0, "2"], [3.0, "4"], [4.0, "4"]])
        );
        assert_eq!(
            evaluate(&db, parse("cpu").unwrap(), 303001, 303001, 1, true).unwrap()["result"],
            json!([])
        );
        assert!(evaluate(&db, parse("cpu").unwrap(), 0, 11000, 1, false).is_err());
    }
    #[test]
    fn time_formats() {
        assert_eq!(timestamp("1970-01-01T00:00:01.250Z").unwrap(), 1250);
        assert_eq!(timestamp("1.25").unwrap(), 1250);
        assert_eq!(step("5s").unwrap(), 5000);
        for s in ["0", "NaN", "-1", "0.0001"] {
            assert!(step(s).is_err());
        }
    }

    #[test]
    fn window_syntax() {
        let input = r#"avg_over_time(cpu{host=")]}["}[1h30m500ms])"#;
        let Expr::Window {
            duration, selector, ..
        } = parse(input).unwrap()
        else {
            panic!("expected window expression");
        };
        assert_eq!(duration, 5_400_500);
        assert!(selector.matches(&LabelSet::from_pairs([
            ("__name__", "cpu"),
            ("host", ")]}["),
        ])));
        for input in [
            "avg_over_time(cpu[0s])",
            "avg_over_time(cpu[2m1h])",
            "avg_over_time(cpu[2m1m])",
            "avg_over_time(cpu[1.5h])",
            "avg_over_time(cpu[5])",
            "avg_over_time(cpu[18446744073709551615s])",
            "avg_over_time(cpu[18446744073709551616ms])",
            "avg_over_time(cpu[5m)",
            "avg_over_time(cpu[5m])extra",
            "avg_over_time(cpu)",
            "rate(cpu[5m])",
            "avg_over_time(sum_over_time(cpu[5m])[5m])",
        ] {
            assert!(parse(input).is_err(), "{input}");
        }
        assert!(matches!(parse("1 + 1"), Ok(Expr::Scalar(2.0))));
    }

    #[test]
    fn window_functions_use_only_samples_inside_the_window() {
        let db = Db::new();
        db.write(WriteBatch {
            series: vec![
                (
                    LabelSet::from_pairs([("__name__", "cpu"), ("host", "a")]),
                    vec![
                        Sample::new(0, 100.0),
                        Sample::new(1000, 4.0),
                        Sample::new(3000, 2.0),
                        Sample::new(4000, 999.0),
                    ],
                ),
                (
                    LabelSet::from_pairs([("__name__", "cpu"), ("host", "b")]),
                    vec![Sample::new(2000, 8.0)],
                ),
            ],
        })
        .unwrap();
        for (function, a, b) in [
            ("avg", "3", "8"),
            ("min", "2", "8"),
            ("max", "4", "8"),
            ("sum", "6", "8"),
            ("count", "2", "1"),
        ] {
            let expr = parse(&format!("{function}_over_time(cpu[3s])")).unwrap();
            let result = evaluate(&db, expr, 3000, 3000, 1, true).unwrap();
            let mut series = result["result"].as_array().unwrap().clone();
            series.sort_by_key(|item| item["metric"]["host"].as_str().unwrap().to_owned());
            assert_eq!(
                series,
                vec![
                    json!({"metric":{"host":"a"},"value":[3.0,a]}),
                    json!({"metric":{"host":"b"},"value":[3.0,b]}),
                ],
                "{function}"
            );
        }
        let expr = parse(r#"count_over_time(cpu{host="a"}[1s])"#).unwrap();
        let result = evaluate(&db, expr, 0, 6000, 1000, false).unwrap();
        assert_eq!(
            result["result"][0]["values"],
            json!([[0.0, "1"], [1.0, "1"], [3.0, "1"], [4.0, "1"]])
        );
        let expr = parse("count_over_time(cpu[1s])").unwrap();
        assert_eq!(
            evaluate(&db, expr, 6000, 6000, 1, true).unwrap()["result"],
            json!([])
        );
    }

    #[test]
    fn window_average_stays_finite_when_the_sum_overflows() {
        let samples = [Sample::new(0, f64::MAX), Sample::new(1, f64::MAX)];
        assert_eq!(WindowFunction::Sum.evaluate(&samples), f64::INFINITY);
        assert_eq!(WindowFunction::Avg.evaluate(&samples), f64::MAX);
    }
}
