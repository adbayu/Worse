use super::{
    de, fmt, Deserialize, Deserializer, MapAccess, Number, OMap, OValue, SeqAccess, Serialize,
    SerializeMap, SerializeSeq, Serializer, Visitor,
};

impl Serialize for OValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Null => serializer.serialize_unit(),
            Self::Bool(b) => serializer.serialize_bool(*b),
            Self::Number(n) => n.serialize(serializer),
            Self::String(s) => serializer.serialize_str(s),
            Self::Array(items) => {
                let mut seq = serializer.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(item)?;
                }
                seq.end()
            }
            Self::Object(map) => {
                let mut out = serializer.serialize_map(Some(map.0.len()))?;
                for (k, v) in &map.0 {
                    out.serialize_entry(k, v)?;
                }
                out.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for OValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(OVisitor)
    }
}

pub(super) struct OVisitor;

impl<'de> Visitor<'de> for OVisitor {
    type Value = OValue;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> Result<OValue, E> {
        Ok(OValue::Bool(v))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<OValue, E> {
        Ok(OValue::Number(v.into()))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<OValue, E> {
        Ok(OValue::Number(v.into()))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<OValue, E> {
        Ok(Number::from_f64(v).map_or(OValue::Null, OValue::Number))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<OValue, E> {
        Ok(OValue::String(v.to_owned()))
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<OValue, E> {
        Ok(OValue::String(v))
    }

    fn visit_unit<E: de::Error>(self) -> Result<OValue, E> {
        Ok(OValue::Null)
    }

    fn visit_none<E: de::Error>(self) -> Result<OValue, E> {
        Ok(OValue::Null)
    }

    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<OValue, D::Error> {
        OValue::deserialize(d)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<OValue, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element()? {
            items.push(item);
        }
        Ok(OValue::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<OValue, A::Error> {
        let mut map = OMap::new();
        while let Some((key, value)) = access.next_entry::<String, OValue>()? {
            map.insert(key, value);
        }
        Ok(OValue::Object(map))
    }
}
