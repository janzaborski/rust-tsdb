use crate::{
    Db,
    model::{Matcher, MatcherOperator, TimeRange},
};
use serde_json::{Value, json};

#[derive(Debug)]
pub enum Expr {
    Selector(String, Vec<(String, String)>),
    Scalar(f64),
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
    let n = s
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
        .unwrap_or(s.len());
    let name = &s[..n];
    if !identifier(name, true) {
        return Err("at byte 0: expected metric name".into());
    }
    let mut rest = s[n..].trim_start();
    let mut labels = Vec::new();
    if rest.is_empty() {
        return Ok(Expr::Selector(name.into(), labels));
    }
    if !rest.starts_with('{') {
        return Err(format!("at byte {n}: expected label selector"));
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
    if rest != "}" {
        return Err(format!(
            "at byte {}: unexpected trailing text",
            s.len() - rest.len() + 1
        ));
    }
    Ok(Expr::Selector(name.into(), labels))
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
        Expr::Selector(name, labels) => {
            let mut matchers = vec![Matcher::new("__name__", name, MatcherOperator::Equal)];
            matchers.extend(
                labels
                    .iter()
                    .filter(|(_, v)| !v.is_empty())
                    .map(|(k, v)| Matcher::new(k.clone(), v.clone(), MatcherOperator::Equal)),
            );
            let series = db
                .query(
                    &matchers,
                    TimeRange::new(start.saturating_sub(300_000), end),
                )
                .map_err(|e| e.to_string())?;
            let mut points = 0;
            for s in series {
                if !labels
                    .iter()
                    .all(|(k, v)| s.labels.get(k).unwrap_or("") == v)
                {
                    continue;
                }
                let mut i = 0;
                let mut values = Vec::new();
                for t in times() {
                    while i < s.samples.len() && s.samples[i].timestamp <= t {
                        i += 1;
                    }
                    if i > 0 && s.samples[i - 1].timestamp >= t.saturating_sub(300_000) {
                        points += 1;
                        if points > 1_000_000 {
                            return Err("query exceeds 1000000 result points".into());
                        }
                        values.push(json!([
                            t as f64 / 1000.0,
                            s.samples[i - 1].value.to_string()
                        ]));
                    }
                }
                if values.is_empty() {
                    continue;
                }
                let metric: serde_json::Map<String, Value> =
                    s.labels.iter().map(|(k, v)| (k.into(), json!(v))).collect();
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
}
