//! Original in-memory span recorder: no global subscriber or formatted error capture.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Dispatch, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::{Layer, Registry};

#[derive(Clone, Debug)]
pub struct Span {
    pub name: &'static str,
    pub parent: Option<u64>,
    pub fields: BTreeMap<String, String>,
    pub initial_fields: BTreeMap<String, String>,
    pub closed: bool,
}
#[derive(Clone, Default)]
pub struct Capture(
    pub Arc<Mutex<BTreeMap<u64, Span>>>,
    pub Arc<Mutex<Vec<String>>>,
);
impl Capture {
    pub fn events(&self) -> Vec<String> {
        self.1.lock().unwrap().clone()
    }
    pub fn dispatch(&self) -> Dispatch {
        Dispatch::new(Registry::default().with(self.clone()))
    }
    pub fn spans(&self) -> Vec<Span> {
        self.0.lock().unwrap().values().cloned().collect()
    }
    pub fn named(&self, name: &str) -> Span {
        let spans = self.spans();
        let found: Vec<_> = spans.into_iter().filter(|s| s.name == name).collect();
        assert_eq!(found.len(), 1, "expected one {name}: {found:?}");
        found.into_iter().next().unwrap()
    }
    pub fn parent_name(&self, span: &Span) -> &'static str {
        self.0.lock().unwrap()[&span.parent.unwrap()].name
    }
}
struct Fields<'a>(&'a mut BTreeMap<String, String>);
impl Visit for Fields<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0.insert(field.name().into(), format!("{value:?}"));
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().into(), value.into());
    }
}
impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for Capture {
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let mut fields = BTreeMap::new();
        attrs.record(&mut Fields(&mut fields));
        let parent = ctx.span(id).unwrap().parent().map(|s| s.id().into_u64());
        self.0.lock().unwrap().insert(
            id.into_u64(),
            Span {
                name: attrs.metadata().name(),
                parent,
                initial_fields: fields.clone(),
                fields,
                closed: false,
            },
        );
    }
    fn on_record(&self, id: &Id, values: &Record<'_>, _: Context<'_, S>) {
        let mut spans = self.0.lock().unwrap();
        values.record(&mut Fields(
            &mut spans.get_mut(&id.into_u64()).unwrap().fields,
        ));
    }
    fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
        if event.metadata().target().starts_with("girt") {
            let mut fields = BTreeMap::new();
            event.record(&mut Fields(&mut fields));
            self.1.lock().unwrap().push(format!("{fields:?}"));
        }
    }
    fn on_close(&self, id: Id, _: Context<'_, S>) {
        self.0
            .lock()
            .unwrap()
            .get_mut(&id.into_u64())
            .unwrap()
            .closed = true;
    }
}
