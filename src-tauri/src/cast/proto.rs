//! CASTV2 protocol message (hand-vendored from Chromium's cast_channel.proto,
//! so we don't need protoc at build time).

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct CastMessage {
    #[prost(int32, tag = "1")]
    pub protocol_version: i32, // always 0 (CASTV2_1_0)
    #[prost(string, tag = "2")]
    pub source_id: String,
    #[prost(string, tag = "3")]
    pub destination_id: String,
    #[prost(string, tag = "4")]
    pub namespace: String,
    #[prost(int32, tag = "5")]
    pub payload_type: i32, // 0 = STRING, 1 = BINARY
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
        protocol_version: 0,
        source_id: "sender-pacto".into(),
        destination_id: dest.into(),
        namespace: namespace.into(),
        payload_type: 0,
        payload_utf8: Some(payload.to_string()),
        payload_binary: None,
    }
}
