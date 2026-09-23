//! Browser source import/export. These callbacks never touch the evaluator.
use std::{cell::RefCell, rc::Rc, sync::mpsc};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use web_sys::{Blob, BlobPropertyBag, HtmlAnchorElement, HtmlInputElement, Url};

pub struct Imported {
    pub request: u64,
    pub previous_source: String,
    pub result: Result<(String, String), String>,
}

pub struct BrowserFiles {
    pub name: String,
    pub replaced_name: Option<String>,
    pub notice: Option<(bool, String)>,
    pub latest_request: u64,
    results: mpsc::Receiver<Imported>,
    outgoing: mpsc::Sender<Imported>,
    pending: Rc<RefCell<(u64, String)>>,
    picker: Option<Picker>,
    downloads: Vec<(String, web_time::Instant)>,
}

struct Picker {
    input: HtmlInputElement,
    _change: Closure<dyn FnMut(web_sys::Event)>,
}
impl Drop for Picker {
    fn drop(&mut self) {
        self.input.set_onchange(None);
        self.input.remove();
    }
}

impl Default for BrowserFiles {
    fn default() -> Self {
        let (outgoing, results) = mpsc::channel();
        Self {
            name: "composition.eod".into(),
            replaced_name: None,
            notice: None,
            latest_request: 0,
            results,
            outgoing,
            pending: Rc::new(RefCell::new((0, String::new()))),
            picker: None,
            downloads: Vec::new(),
        }
    }
}

fn document() -> Result<web_sys::Document, String> {
    web_sys::window()
        .and_then(|window| window.document())
        .ok_or_else(|| "browser document is unavailable".into())
}
fn message(error: impl Into<JsValue>) -> String {
    let error = error.into();
    error.as_string().unwrap_or_else(|| format!("{error:?}"))
}

impl BrowserFiles {
    pub fn import(&mut self, source: &str) -> Result<(), String> {
        if self.picker.is_none() {
            let document = document()?;
            let input: HtmlInputElement = document
                .create_element("input")
                .map_err(message)?
                .dyn_into()
                .map_err(message)?;
            input.set_type("file");
            input.set_accept(".eod,.lua,text/plain");
            input.set_attribute("hidden", "").map_err(message)?;
            let outgoing = self.outgoing.clone();
            let pending = self.pending.clone();
            let change = Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
                let Some(input) = event
                    .target()
                    .and_then(|target| target.dyn_into::<HtmlInputElement>().ok())
                else {
                    return;
                };
                let Some(file) = input.files().and_then(|files| files.get(0)) else {
                    return;
                };
                let (request, previous_source) = pending.borrow().clone();
                let outgoing = outgoing.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    let limit = apteronotus_lua::Limits::default().source_bytes;
                    let result = if file.size() > limit as f64 {
                        Err(format!("source exceeds the {limit}-byte document limit"))
                    } else {
                        match wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await {
                            Ok(buffer) => {
                                let bytes = js_sys::Uint8Array::new(&buffer).to_vec();
                                String::from_utf8(bytes)
                                    .map(|source| (file.name(), source))
                                    .map_err(|error| error.to_string())
                            }
                            Err(error) => Err(message(error)),
                        }
                    };
                    let _ = outgoing.send(Imported {
                        request,
                        previous_source,
                        result,
                    });
                });
            });
            input.set_onchange(Some(change.as_ref().unchecked_ref()));
            document
                .body()
                .ok_or("browser body is unavailable")?
                .append_child(&input)
                .map_err(message)?;
            self.picker = Some(Picker {
                input,
                _change: change,
            });
        }
        self.latest_request = self.latest_request.wrapping_add(1);
        *self.pending.borrow_mut() = (self.latest_request, source.to_string());
        if let Some(picker) = &self.picker {
            picker.input.set_value("");
            picker.input.click();
        }
        Ok(())
    }

    pub fn take_import(&mut self) -> Option<Imported> {
        self.downloads.retain(|(url, created)| {
            if created.elapsed().as_secs() >= 60 {
                let _ = Url::revoke_object_url(url);
                false
            } else {
                true
            }
        });
        self.results
            .try_iter()
            .filter(|result| result.request == self.latest_request)
            .last()
    }

    pub fn export(&mut self, source: &str) -> Result<(), String> {
        let document = document()?;
        let anchor: HtmlAnchorElement = document
            .create_element("a")
            .map_err(message)?
            .dyn_into()
            .map_err(message)?;
        let parts = js_sys::Array::new();
        parts.push(&JsValue::from_str(source));
        let options = BlobPropertyBag::new();
        options.set_type("text/plain;charset=utf-8");
        let blob = Blob::new_with_str_sequence_and_options(&parts, &options).map_err(message)?;
        anchor.set_download(if self.name.trim().is_empty() {
            "composition.eod"
        } else {
            &self.name
        });
        anchor.set_attribute("hidden", "").map_err(message)?;
        document
            .body()
            .ok_or("browser body is unavailable")?
            .append_child(&anchor)
            .map_err(message)?;
        let url = match Url::create_object_url_with_blob(&blob) {
            Ok(url) => url,
            Err(error) => {
                anchor.remove();
                return Err(message(error));
            }
        };
        anchor.set_href(&url);
        anchor.click();
        anchor.remove();
        // Keep the blob alive while the browser starts its download. The next
        // UI polls release old URLs; removing the app releases any still held.
        self.downloads.push((url, web_time::Instant::now()));
        Ok(())
    }
}

impl Drop for BrowserFiles {
    fn drop(&mut self) {
        for (url, _) in self.downloads.drain(..) {
            let _ = Url::revoke_object_url(&url);
        }
    }
}
