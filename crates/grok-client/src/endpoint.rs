use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};

use crate::{GrokError, Result};

const PATH_SEGMENT_ENCODE_SET: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'`')
    .add(b'{')
    .add(b'}')
    .add(b'/')
    .add(b'&');

const QUERY_ENCODE_SET: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'<')
    .add(b'>')
    .add(b'`')
    .add(b'{')
    .add(b'}')
    .add(b'&');

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueryItem {
    pub name: String,
    pub value: String,
}

impl QueryItem {
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

pub fn encoded_path_segment(segment: &str) -> String {
    utf8_percent_encode(segment, PATH_SEGMENT_ENCODE_SET).to_string()
}

pub fn endpoint_path(segments: &[impl AsRef<str>], query_items: &[QueryItem]) -> Result<String> {
    if segments.is_empty() {
        return Err(GrokError::Api(
            "Endpoint path requires at least one segment".to_string(),
        ));
    }

    let mut path = String::from("/");
    path.push_str(
        &segments
            .iter()
            .map(|segment| encoded_path_segment(segment.as_ref()))
            .collect::<Vec<_>>()
            .join("/"),
    );

    if query_items.is_empty() {
        return Ok(path);
    }

    path.push('?');
    path.push_str(
        &query_items
            .iter()
            .map(|item| {
                format!(
                    "{}={}",
                    utf8_percent_encode(&item.name, QUERY_ENCODE_SET),
                    utf8_percent_encode(&item.value, QUERY_ENCODE_SET)
                )
            })
            .collect::<Vec<_>>()
            .join("&"),
    );
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::{QueryItem, endpoint_path};
    use crate::Result;

    #[test]
    fn builds_encoded_endpoint_paths() -> Result<()> {
        let path = endpoint_path(
            &["conversations", "one/two?three&four", "response-node"],
            &[
                QueryItem::new("cursor", "a b"),
                QueryItem::new("raw", "x/y"),
            ],
        )?;

        assert_eq!(
            path,
            "/conversations/one%2Ftwo%3Fthree%26four/response-node?cursor=a%20b&raw=x/y"
        );
        Ok(())
    }
}
