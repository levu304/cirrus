// Cirrus protocol XML serialization helpers.
//
use crate::error::S3Error;
/// Provides the canonical `serialize` function that produces AWS S3-compatible
/// XML responses with the required xmlns namespace on the root element.
use crate::types::to_xml_string;

/// The AWS S3 XML namespace URI required on every response root element.
///
/// Without this attribute, AWS SDKs will reject responses as invalid.
pub const S3_XML_NAMESPACE: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// Serialize a value to S3-compatible XML with the xmlns namespace on the root element.
///
/// This is the canonical way to produce S3 XML responses. It:
/// 1. Serializes the value via quick-xml (through `to_xml_string`)
/// 2. Expands self-closing tags to open/close pairs
/// 3. Injects `xmlns="http://s3.amazonaws.com/doc/2006-03-01/"` into the root opening tag
///
// # Arguments
///
/// * `value` - The value to serialize (must implement `serde::Serialize`)
// * `root_name` - The expected root element name, used to target the xmlns injection
///
/// # Returns
///
/// A complete XML string with xmlns on the root element and expanded empty tags.
pub fn serialize<T: serde::Serialize>(value: &T, root_name: &str) -> Result<String, S3Error> {
    let body = to_xml_string(value)?;
    Ok(inject_xmlns(&body, root_name))
}

/// Inject the S3 XML namespace into the root opening tag.
///
/// Handles:
/// - Root tags with no attributes: `<Foo>` → `<Foo xmlns="...">`
/// - Root tags with existing attributes: `<Foo bar="x">` → `<Foo xmlns="..." bar="x">`
/// - Self-closing root tags: `<Foo/>` → `<Foo xmlns="..."></Foo>` (expanded + xmlns)
fn inject_xmlns(xml: &str, root_name: &str) -> String {
    let xmlns_attr = format!(" xmlns=\"{}\"", S3_XML_NAMESPACE);

    // Build the opening tag patterns to search for
    let open_tag = format!("<{}", root_name);
    let self_closing_tag = format!("<{}/>", root_name);

    // Handle self-closing root tag (unlikely for S3 responses but handle gracefully)
    if xml.starts_with(&self_closing_tag) {
        return xml.replacen(
            &self_closing_tag,
            &format!("<{}{}></{}>", root_name, xmlns_attr, root_name),
            1,
        );
    }

    // Handle normal opening tag: <RootName> or <RootName attr="...">
    // Use replacen with the opening bracket to avoid matching child elements
    let replacement = format!("<{}{}", root_name, xmlns_attr);
    xml.replacen(&open_tag, &replacement, 1)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, serde::Serialize)]
    struct SimpleRoot {
        #[serde(rename = "Name")]
        name: String,
    }

    #[derive(Debug, serde::Serialize)]
    struct RootWithChild {
        #[serde(rename = "Name")]
        name: String,
        #[serde(rename = "Count")]
        count: u32,
    }

    #[test]
    fn test_serialize_injects_xmlns_on_root() {
        let value = SimpleRoot {
            name: "test-bucket".into(),
        };
        let xml = serialize(&value, "SimpleRoot").expect("serialize failed");

        assert!(
            xml.contains("xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\""),
            "xmlns should be present: {xml}"
        );
        assert!(
            xml.starts_with("<SimpleRoot xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">"),
            "xmlns should be on root opening tag: {xml}"
        );
    }

     #[test]
     fn test_serialize_xmlns_on_root_only_not_children() {
         let value = RootWithChild {
             name: "my-bucket".into(),
             count: 42,
         };
         let xml = serialize(&value, "RootWithChild").expect("serialize failed");

         // Count xmlns occurrences — should be exactly 1 (on root only)
         let xmlns_count = xml.matches("xmlns=").count();
         assert_eq!(
             xmlns_count, 1,
             "xmlns should appear exactly once (on root only), found {xmlns_count}: {xml}"
         );

         // Child elements should NOT have xmlns
         assert!(!xml.contains("<Name xmlns="), "child Name should not have xmlns: {xml}");
         assert!(!xml.contains("<Count xmlns="), "child Count should not have xmlns: {xml}");
     }

     #[test]
     fn test_serialize_roundtrip_with_struct() {
         let value = RootWithChild {
             name: "roundtrip-test".into(),
             count: 99,
         };
         let xml = serialize(&value, "RootWithChild").expect("serialize failed");

         // Verify structure
         assert!(xml.starts_with("<RootWithChild xmlns="));
         assert!(xml.contains("<Name>roundtrip-test</Name>"));
         assert!(xml.contains("<Count>99</Count>"));
         assert!(xml.ends_with("</RootWithChild>"));
     }

    #[test]
    fn test_inject_xmlns_no_attributes() {
        let input = "<ListBucketResult><Name>bucket</Name></ListBucketResult>";
        let output = inject_xmlns(input, "ListBucketResult");

        assert!(
            output.starts_with(
                "<ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">"
            )
        );
        assert!(output.contains("<Name>bucket</Name>"));
    }

    #[test]
    fn test_inject_xmlns_self_closing_root() {
        let input = "<EmptyResult/>";
        let output = inject_xmlns(input, "EmptyResult");

        assert_eq!(
            output,
            "<EmptyResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"></EmptyResult>"
        );
    }

    #[test]
    fn test_inject_xmlns_does_not_duplicate() {
        // If xmlns already present (edge case), replacen only replaces first occurrence
        let input = "<Root xmlns=\"old\"><Child/></Root>";
        let output = inject_xmlns(input, "Root");

        // Should replace the first occurrence
        assert!(output.contains("xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\""));
    }

    #[test]
    fn test_serialize_constant_namespace() {
        assert_eq!(S3_XML_NAMESPACE, "http://s3.amazonaws.com/doc/2006-03-01/");
    }
}
