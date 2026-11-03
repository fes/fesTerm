use crate::{
    platform,
    protocol::{HostRequest, HostResult, Limits, Outcome, MAX_PENDING_CALLS},
    snapshot::{ApprovedBundle, ParameterValue, Parameters},
    HEAP_BYTES, STACK_BYTES,
};
use rquickjs::{
    loader::{ImportAttributes, Loader, Resolver},
    module::Declared,
    promise::PromiseState,
    Context, Ctx, Function, Module, Object, Promise, Runtime, Value,
};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet, VecDeque},
    rc::Rc,
    time::{Duration, Instant},
};

pub(crate) enum Input {
    Admitted(u32),
    Result(u32, HostResult),
    Cancel,
    Idle,
}

pub(crate) trait Transport {
    fn send(&mut self, id: u32, request: HostRequest) -> Result<(), crate::Error>;
    fn receive(&mut self) -> Result<Input, crate::Error>;
}

struct Sources(BTreeMap<String, String>);
impl Loader for Sources {
    fn load<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        name: &str,
        attributes: Option<ImportAttributes<'js>>,
    ) -> rquickjs::Result<Module<'js, Declared>> {
        if attributes.is_some() {
            return Err(rquickjs::Error::new_loading("denied"));
        }
        let source = self
            .0
            .get(name)
            .ok_or_else(|| rquickjs::Error::new_loading("denied"))?;
        Module::declare(ctx.clone(), name, source.as_bytes())
    }
}

struct Names(std::collections::BTreeSet<String>);
impl Resolver for Names {
    fn resolve<'js>(
        &mut self,
        _: &Ctx<'js>,
        _: &str,
        name: &str,
        attributes: Option<ImportAttributes<'js>>,
    ) -> rquickjs::Result<String> {
        if attributes.is_none() && self.0.contains(name) {
            Ok(name.to_owned())
        } else {
            Err(rquickjs::Error::new_resolving("denied", "denied"))
        }
    }
}

struct Pending<'js> {
    request: HostRequest,
    resolve: Function<'js>,
    reject: Function<'js>,
}

struct PendingCleanup<'js>(Rc<RefCell<BTreeMap<u32, Pending<'js>>>>);

impl Drop for PendingCleanup<'_> {
    fn drop(&mut self) {
        self.0.borrow_mut().clear();
    }
}

const BOOTSTRAP: &str = r#"
(() => {
    "use strict";
    const call = globalThis.__festerm_call;
    delete globalThis.__festerm_call;
    const stringify = JSON.stringify;
    const request = value => call(stringify(value));
    const methods = {};
    for (const name of ["observe", "readScreen", "readScrollback", "getSelection",
                         "waitForChange", "sendText", "sendKeys", "open", "invokeWorkflow"]) {
        methods[name] = async options => request({...options, method: name});
    }
    const host = Object.freeze(methods);
    // No runtime source generation: otherwise eval/constructor-produced
    // import() could evade the source AST qualification.
    for (const fn of [function(){}, async function(){}, function*(){}, async function*(){}]) {
        Object.defineProperty(Object.getPrototypeOf(fn), "constructor",
            {value: undefined, writable: false, configurable: false});
    }
    for (const name of ["eval", "Function"]) {
        Object.defineProperty(globalThis, name,
            {value: undefined, writable: false, configurable: false});
    }
    return {
        host,
        invoke: async (main, args) => await main(args),
    };
})()
"#;

/// Private: production callers can reach this only through worker_entry after
/// native confinement. Unit tests qualify the engine in owned test processes.
pub(crate) fn run(
    bundle: &ApprovedBundle,
    parameters: &Parameters,
    limits: &Limits,
    transport: &mut impl Transport,
) -> Outcome {
    if limits.validate().is_err() || bundle.validate_parameters(parameters).is_err() {
        return Outcome::ProtocolFailed {};
    }
    let start = Instant::now();
    let Some(cpu_start) = platform::cpu_time() else {
        return Outcome::ResourceLimit {};
    };
    let cpu_limit = Duration::from_millis(limits.cpu_ms);
    let wall_limit = Duration::from_millis(limits.run_ms);
    let exhausted = Rc::new(Cell::new(false));
    let interrupted = exhausted.clone();
    let Ok(runtime) = Runtime::new() else {
        return Outcome::ResourceLimit {};
    };
    runtime.set_memory_limit(HEAP_BYTES);
    runtime.set_max_stack_size(STACK_BYTES);
    runtime.set_interrupt_handler(Some(Box::new(move || {
        let stop = platform::cpu_time()
            .is_none_or(|now| now.saturating_sub(cpu_start) >= cpu_limit)
            || start.elapsed() >= wall_limit;
        interrupted.set(stop);
        stop
    })));
    runtime.set_loader(
        Names(bundle.snapshot().modules.keys().cloned().collect()),
        Sources(bundle.snapshot().modules.clone()),
    );
    let Ok(context) = Context::full(&runtime) else {
        return Outcome::ResourceLimit {};
    };
    let result = context.with(|ctx| -> rquickjs::Result<Outcome> {
        let pending: Rc<RefCell<BTreeMap<u32, Pending<'_>>>> =
            Rc::new(RefCell::new(BTreeMap::new()));
        let _pending_cleanup = PendingCleanup(pending.clone());
        let outgoing = Rc::new(RefCell::new(VecDeque::new()));
        let next_id = Rc::new(Cell::new(0u32));
        let closed = Rc::new(Cell::new(false));
        let calls = pending.clone();
        let queue = outgoing.clone();
        let ids = next_id.clone();
        let admission = closed.clone();
        let max_calls = u32::from(limits.max_host_calls);
        let callback_ctx = ctx.clone();
        let call = Function::new(ctx.clone(), move |json: String| {
            if admission.get() || json.len() > crate::protocol::MAX_FRAME_BYTES {
                return Err(rquickjs::Error::new_from_js("host", "closed"));
            }
            let request: HostRequest = serde_json::from_str(&json)
                .map_err(|_| rquickjs::Error::new_from_js("host", "request"))?;
            request
                .validate()
                .map_err(|_| rquickjs::Error::new_from_js("host", "request"))?;
            if calls.borrow().len() >= MAX_PENDING_CALLS || ids.get() >= max_calls {
                return Err(rquickjs::Error::new_from_js("host", "quota"));
            }
            let id = ids.get() + 1;
            ids.set(id);
            let (promise, resolve, reject) = Promise::new(&callback_ctx)?;
            calls.borrow_mut().insert(
                id,
                Pending {
                    request: request.clone(),
                    resolve,
                    reject,
                },
            );
            queue.borrow_mut().push_back((id, request));
            Ok(promise)
        })?;
        ctx.globals().set("__festerm_call", call)?;
        let bootstrap: Object = ctx.eval(BOOTSTRAP)?;
        let host: Object = bootstrap.get("host")?;
        let invoke: Function = bootstrap.get("invoke")?;
        let module = Module::declare(
            ctx.clone(),
            bundle.manifest().entry.as_str(),
            bundle.snapshot().modules[&bundle.manifest().entry].as_bytes(),
        )?;
        let (module, evaluated) = module.eval()?;
        // Top-level await is deliberately outside this v1 profile. Module
        // initialization must finish without host effects before main().
        for _ in 0..1_024 {
            if evaluated.state() != PromiseState::Pending {
                break;
            }
            if !ctx.execute_pending_job() {
                break;
            }
        }
        if evaluated.state() == PromiseState::Pending {
            return Ok(Outcome::ScriptFailed {});
        }
        evaluated
            .result::<()>()
            .ok_or(rquickjs::Error::Exception)??;
        let main: Function = module.get("main")?;
        let mut values = serde_json::Map::new();
        for (key, value) in parameters {
            let v = match value {
                ParameterValue::Text(v) | ParameterValue::Target(v) => {
                    serde_json::Value::String(v.clone())
                }
                ParameterValue::Integer(v) => serde_json::Value::from(*v),
                ParameterValue::Boolean(v) => serde_json::Value::Bool(*v),
            };
            values.insert(key.clone(), v);
        }
        let args = Object::new(ctx.clone())?;
        args.set("fesTerm", host)?;
        args.set(
            "params",
            ctx.json_parse(serde_json::to_vec(&values).map_err(|_| rquickjs::Error::Exception)?)?,
        )?;
        let main_promise: Promise = invoke.call((main, args))?;
        let mut dispatched = BTreeSet::new();
        let mut unadmitted = BTreeSet::new();
        let mut settled = VecDeque::new();
        loop {
            if start.elapsed() >= wall_limit
                || platform::cpu_time().is_none_or(|now| now.saturating_sub(cpu_start) >= cpu_limit)
            {
                return Ok(Outcome::ResourceLimit {});
            }
            if main_promise.state() != PromiseState::Pending {
                // This is the completion cut: no new requests, no draining
                // microtasks after root settlement, no script getter effects.
                closed.set(true);
                if !pending.borrow().is_empty() {
                    return Ok(Outcome::UnsettledWork {});
                }
                let value = main_promise
                    .result::<Value>()
                    .ok_or(rquickjs::Error::Exception)??;
                let json = match ctx.json_stringify(value)? {
                    Some(value) => value.to_string()?,
                    None => "null".into(),
                };
                if json.len() > 49_152 {
                    return Ok(Outcome::ResourceLimit {});
                }
                let value = serde_json::from_str(&json).map_err(|_| rquickjs::Error::Exception)?;
                if crate::protocol::validate_value(&value).is_err() {
                    return Ok(Outcome::ResourceLimit {});
                }
                return Ok(Outcome::Completed { value });
            }
            if unadmitted.is_empty() {
                // Preserve Promise job order between separately received
                // results, while still cutting off newly queued host calls
                // if the root settles during those jobs.
                if ctx.execute_pending_job() {
                    continue;
                }
                while let Some((id, request)) = outgoing.borrow_mut().pop_front() {
                    if transport.send(id, request).is_err() {
                        return Ok(Outcome::ProtocolFailed {});
                    }
                    dispatched.insert(id);
                    unadmitted.insert(id);
                }
                if !unadmitted.is_empty() {
                    continue;
                }
                if let Some((id, result)) = settled.pop_front() {
                    let Some(call) = pending.borrow_mut().remove(&id) else {
                        return Ok(Outcome::ProtocolFailed {});
                    };
                    if let HostResult::Error { code } = result {
                        // Only the typed content-free code enters a rejection.
                        call.reject.call::<_, ()>((format!("{code:?}"),))?;
                    } else {
                        let value = ctx.json_parse(
                            serde_json::to_vec(&result).map_err(|_| rquickjs::Error::Exception)?,
                        )?;
                        call.resolve.call::<_, ()>((value,))?;
                    }
                    continue;
                }
            }
            match transport.receive() {
                Ok(Input::Cancel) => return Ok(Outcome::Cancelled {}),
                Ok(Input::Idle) => {
                    if pending.borrow().is_empty() {
                        return Ok(Outcome::UnsettledWork {});
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Ok(Input::Admitted(id)) => {
                    if !unadmitted.remove(&id) {
                        return Ok(Outcome::ProtocolFailed {});
                    }
                }
                Ok(Input::Result(id, result)) => {
                    if !dispatched.remove(&id)
                        || pending
                            .borrow()
                            .get(&id)
                            .is_none_or(|call| result.validate_for(&call.request).is_err())
                    {
                        return Ok(Outcome::ProtocolFailed {});
                    }
                    unadmitted.remove(&id);
                    settled.push_back((id, result));
                }
                Err(_) => return Ok(Outcome::ProtocolFailed {}),
            }
        }
    });
    match result {
        Ok(outcome) => outcome,
        Err(_) if exhausted.get() => Outcome::ResourceLimit {},
        // Exception strings/stacks are never formatted, logged or put on the
        // diagnostics wire, even for syntax errors, rejection values or OOM.
        Err(_) => Outcome::ScriptFailed {},
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::snapshot::fixture;

    #[derive(Default)]
    struct Broker {
        calls: Vec<HostRequest>,
        replies: VecDeque<(u32, HostResult)>,
    }
    impl Transport for Broker {
        fn send(&mut self, id: u32, request: HostRequest) -> Result<(), crate::Error> {
            self.calls.push(request);
            self.replies
                .push_back((id, HostResult::Observation { targets: vec![] }));
            Ok(())
        }
        fn receive(&mut self) -> Result<Input, crate::Error> {
            Ok(self
                .replies
                .pop_front()
                .map_or(Input::Idle, |(id, result)| Input::Result(id, result)))
        }
    }

    #[test]
    fn modules_async_host_round_trip_and_no_ambient_apis() {
        let bundle = fixture(
            r#"
            export async function main({fesTerm}) {
                const result = await fesTerm.observe();
                return [result.targets.length, typeof process, typeof fetch, typeof require,
                    typeof eval, typeof Function, typeof (()=>{}).constructor,
                    typeof (async()=>{}).constructor];
            }

        "#,
        );
        let mut broker = Broker::default();
        let outcome = run(&bundle, &Parameters::new(), &Limits::default(), &mut broker);
        assert_eq!(
            outcome,
            Outcome::Completed {
                value: serde_json::json!([
                    0,
                    "undefined",
                    "undefined",
                    "undefined",
                    "undefined",
                    "undefined",
                    "undefined",
                    "undefined"
                ])
            }
        );
        assert_eq!(broker.calls, [HostRequest::Observe {}]);
    }

    #[test]
    fn static_module_graph_and_async_exports_execute_only_from_snapshot() {
        let mut snapshot = fixture("export function main(){}").snapshot().clone();
        snapshot.manifest.modules.insert(0, "dep.js".into());
        snapshot.modules.insert(
            "main.js".into(),
            "import {answer} from 'dep.js'; export async function main(){ return await answer(); }"
                .into(),
        );
        snapshot.modules.insert(
            "dep.js".into(),
            "export async function answer(){ return 42; }".into(),
        );
        let bundle = ApprovedBundle::new(snapshot).unwrap();
        assert_eq!(
            run(
                &bundle,
                &Parameters::new(),
                &Limits::default(),
                &mut Broker::default()
            ),
            Outcome::Completed { value: 42.into() }
        );
    }

    #[test]
    fn malformed_host_calls_still_return_promises_and_no_authority() {
        let source = r#"
            export async function main({fesTerm}) {
                const p = fesTerm.readScreen({approve: true});
                const promise = p instanceof Promise;
                await p.catch(()=>null);
                return promise;
            }
        "#;
        let mut broker = Broker::default();
        assert_eq!(
            run(
                &fixture(source),
                &Parameters::new(),
                &Limits::default(),
                &mut broker
            ),
            Outcome::Completed { value: true.into() }
        );
        assert!(broker.calls.is_empty());
    }

    #[test]
    fn unsettled_host_calls_never_dispatch_after_completion() {
        let bundle =
            fixture("export async function main({fesTerm}) { fesTerm.observe(); return 1; }");
        let mut broker = Broker::default();
        assert_eq!(
            run(&bundle, &Parameters::new(), &Limits::default(), &mut broker),
            Outcome::UnsettledWork {}
        );
        assert!(broker.calls.is_empty());
        let bundle = fixture("export function main(){throw new Error('PRIVATE-SCRIPT-DATA')}");
        assert_eq!(
            run(&bundle, &Parameters::new(), &Limits::default(), &mut broker),
            Outcome::ScriptFailed {}
        );
    }

    #[test]
    fn root_cannot_settle_while_a_forwarded_call_is_unadmitted() {
        struct DelayedAdmission {
            calls: Vec<u32>,
            step: u8,
            acknowledged: bool,
        }

        impl Transport for DelayedAdmission {
            fn send(&mut self, id: u32, _: HostRequest) -> Result<(), crate::Error> {
                self.calls.push(id);
                Ok(())
            }

            fn receive(&mut self) -> Result<Input, crate::Error> {
                assert_eq!(self.calls, [1, 2]);
                self.step += 1;
                Ok(match self.step {
                    1 => Input::Result(2, HostResult::Observation { targets: vec![] }),
                    2 => Input::Idle,
                    3 => {
                        self.acknowledged = true;
                        Input::Admitted(1)
                    }
                    _ => Input::Idle,
                })
            }
        }

        let bundle = fixture(
            "export async function main({fesTerm}) { \
             fesTerm.observe(); await fesTerm.observe(); return 1; }",
        );
        let mut transport = DelayedAdmission {
            calls: Vec::new(),
            step: 0,
            acknowledged: false,
        };
        assert_eq!(
            run(
                &bundle,
                &Parameters::new(),
                &Limits::default(),
                &mut transport
            ),
            Outcome::UnsettledWork {}
        );
        assert!(transport.acknowledged);
    }

    #[test]
    fn admitted_results_preserve_promise_job_order() {
        let bundle = fixture(
            r#"
            export async function main({fesTerm}) {
                const first = fesTerm.observe().then(() => "first").then(value => value);
                const second = fesTerm.observe().then(() => "second");
                const winner = Promise.race([first, second]);
                await Promise.all([first, second]);
                return await winner;
            }
            "#,
        );
        assert_eq!(
            run(
                &bundle,
                &Parameters::new(),
                &Limits::default(),
                &mut Broker::default()
            ),
            Outcome::Completed {
                value: "first".into()
            }
        );
    }

    #[test]
    fn unknown_or_duplicate_host_admissions_fail_closed() {
        struct InvalidAdmissions(VecDeque<u32>);
        impl Transport for InvalidAdmissions {
            fn send(&mut self, _: u32, _: HostRequest) -> Result<(), crate::Error> {
                Ok(())
            }

            fn receive(&mut self) -> Result<Input, crate::Error> {
                Ok(self.0.pop_front().map_or(Input::Idle, Input::Admitted))
            }
        }

        let bundle = fixture("export async function main({fesTerm}) { await fesTerm.observe(); }");
        for ids in [vec![2], vec![1, 1]] {
            let mut transport = InvalidAdmissions(ids.into());
            assert_eq!(
                run(
                    &bundle,
                    &Parameters::new(),
                    &Limits::default(),
                    &mut transport
                ),
                Outcome::ProtocolFailed {}
            );
        }
    }

    #[test]
    fn cancellation_cleans_up_awaited_host_promises() {
        struct Cancel;
        impl Transport for Cancel {
            fn send(&mut self, _: u32, _: HostRequest) -> Result<(), crate::Error> {
                Ok(())
            }

            fn receive(&mut self) -> Result<Input, crate::Error> {
                Ok(Input::Cancel)
            }
        }

        assert_eq!(
            run(
                &fixture("export async function main({fesTerm}) { await fesTerm.observe(); }"),
                &Parameters::new(),
                &Limits::default(),
                &mut Cancel,
            ),
            Outcome::Cancelled {}
        );
    }

    #[test]
    fn loops_recursion_allocations_regex_and_microtasks_are_bounded() {
        for source in [
            "export function main(){while(true){}}",
            "export function main(){function f(){f()} f()}",
            "export function main(){const a=[]; while(true)a.push(new Uint8Array(1024*1024))}",
            "export function main(){return /^(a+)+$/.test('a'.repeat(100)+'!')}",
            "export async function main(){while(true)await Promise.resolve()}",
        ] {
            let limits = Limits {
                cpu_ms: 100,
                run_ms: 200,
                ..Limits::default()
            };
            let start = Instant::now();
            let result = run(
                &fixture(source),
                &Parameters::new(),
                &limits,
                &mut Broker::default(),
            );
            assert!(
                matches!(result, Outcome::ResourceLimit {} | Outcome::ScriptFailed {}),
                "{result:?}"
            );
            assert!(start.elapsed() < Duration::from_secs(2));
        }
    }
}
