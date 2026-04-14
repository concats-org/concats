use serde::Serialize;
use wasm_bindgen::prelude::*;

use crate::{Snapshot, Turn};

fn to_js<T: Serialize>(value: &T) -> JsValue {
    let serializer = serde_wasm_bindgen::Serializer::new().serialize_missing_as_null(true);
    value
        .serialize(&serializer)
        .expect("serialization of parsed message must not fail")
}

#[wasm_bindgen]
pub fn parse_turn(input: &str) -> JsValue {
    match input.parse::<Turn>() {
        Ok(turn) => to_js(&turn),
        Err(_) => JsValue::NULL,
    }
}

#[wasm_bindgen]
pub fn parse_snapshot(input: &str) -> JsValue {
    match input.parse::<Snapshot>() {
        Ok(snapshot) => to_js(&snapshot),
        Err(_) => JsValue::NULL,
    }
}

#[wasm_bindgen]
pub fn suggest_subject(input: &str) -> Option<String> {
    input.parse::<Turn>().ok()?.suggest_subject()
}
