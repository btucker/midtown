use serde::{Deserialize, Serialize};

#[derive(Debug, Default)]
pub struct CooldownTracker {}

impl Serialize for CooldownTracker {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        s.serialize_struct("CooldownTracker", 0)?.end()
    }
}

impl<'de> Deserialize<'de> for CooldownTracker {
    fn deserialize<D: serde::Deserializer<'de>>(_d: D) -> Result<Self, D::Error> {
        Ok(Self::default())
    }
}
