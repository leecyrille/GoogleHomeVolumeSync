//! CASTV2 protocol message (hand-vendored from Chromium's cast_channel.proto,
//! so we don't need protoc at build time).

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct CastMessage {
    // proto2 REQUIRED field: must always be present on the wire, so model it
    // as optional (prost always encodes Some, but would skip a bare 0).
    #[prost(int32, optional, tag = "1")]
    pub protocol_version: Option<i32>, // always Some(0) (CASTV2_1_0)
    #[prost(string, tag = "2")]
    pub source_id: String,
    #[prost(string, tag = "3")]
    pub destination_id: String,
    #[prost(string, tag = "4")]
    pub namespace: String,
    #[prost(int32, optional, tag = "5")]
    pub payload_type: Option<i32>, // Some(0) = STRING, Some(1) = BINARY (proto2 required)
    #[prost(string, optional, tag = "6")]
    pub payload_utf8: Option<String>,
    #[prost(bytes = "vec", optional, tag = "7")]
    pub payload_binary: Option<Vec<u8>>,
}

pub const NS_CONNECTION: &str = "urn:x-cast:com.google.cast.tp.connection";
pub const NS_HEARTBEAT: &str = "urn:x-cast:com.google.cast.tp.heartbeat";
pub const NS_RECEIVER: &str = "urn:x-cast:com.google.cast.receiver";
pub const NS_MEDIA: &str = "urn:x-cast:com.google.cast.media";

pub fn text_msg(dest: &str, namespace: &str, payload: &serde_json::Value) -> CastMessage {
    CastMessage {
        protocol_version: Some(0),
        source_id: "sender-0".into(),
        destination_id: dest.into(),
        namespace: namespace.into(),
        payload_type: Some(0),
        payload_utf8: Some(payload.to_string()),
        payload_binary: None,
    }
}
