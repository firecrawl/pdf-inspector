#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn process_pdf_bytes(data: &[u8]) -> Result<String, JsValue> {
    console_error_panic_hook::set_once();

    let result = crate::process_pdf_mem(data).map_err(|e| JsValue::from_str(&e.to_string()))?;
    
    if let Some(markdown) = result.markdown {
        Ok(markdown)
    } else {
        Ok(String::new())
    }
}
