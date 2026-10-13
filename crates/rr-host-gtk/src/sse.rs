//! Server-Sent Events, read a line at a time (`GET /control/events`).

/// One event: its name (`message` when the stream gives none) and its data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SseEvent {
    pub event: String,
    pub data: String,
}

/// Lines in, events out: `event:` and `data:` lines build an event, a blank line ends it, a line
/// starting with `:` is a comment (the host's `: keepalive`).
#[derive(Default)]
pub struct SseParser {
    event: String,
    data: Vec<String>,
}

impl SseParser {
    pub fn new() -> SseParser {
        SseParser::default()
    }

    /// A line (with or without its `\n` / `\r\n`): the event it ends, if it ends one.
    pub fn line(&mut self, line: &str) -> Option<SseEvent> {
        let line = line.strip_suffix('\n').unwrap_or(line);
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            if self.data.is_empty() && self.event.is_empty() {
                return None;
            }
            let event = SseEvent {
                event: if self.event.is_empty() {
                    "message".into()
                } else {
                    std::mem::take(&mut self.event)
                },
                data: std::mem::take(&mut self.data).join("\n"),
            };
            self.event.clear();
            return Some(event);
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = value.to_owned(),
            "data" => self.data.push(value.to_owned()),
            _ => {} // id, retry: not used by the host
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(text: &str) -> Vec<SseEvent> {
        let mut p = SseParser::new();
        text.split_inclusive('\n').filter_map(|l| p.line(l)).collect()
    }

    #[test]
    fn events_comments_and_crlf() {
        let got = feed("event: state\ndata: {\"a\":1}\n\n: keepalive\n\nevent: viewers\r\ndata: {\"viewers\":[]}\r\n\r\n");
        assert_eq!(
            got,
            vec![
                SseEvent {
                    event: "state".into(),
                    data: "{\"a\":1}".into()
                },
                SseEvent {
                    event: "viewers".into(),
                    data: "{\"viewers\":[]}".into()
                },
            ]
        );
    }

    #[test]
    fn data_lines_join_and_unnamed_is_message() {
        let got = feed("data: one\ndata:two\n\n");
        assert_eq!(
            got,
            vec![SseEvent {
                event: "message".into(),
                data: "one\ntwo".into()
            }]
        );
    }

    #[test]
    fn an_unfinished_event_is_not_given() {
        assert!(feed("event: state\ndata: {}\n").is_empty());
    }
}
