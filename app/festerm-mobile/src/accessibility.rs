//! Bridges egui's AccessKit tree to the native accessibility adapter
//! (ADR 0075). eframe's own AccessKit integration cannot be used: its Android
//! adapter requires GameActivity, and it has no iOS adapter.
//!
//! Tree generation starts only after a platform client (VoiceOver, TalkBack,
//! UI automation) requests the tree; idle sessions do not build it per frame.

use eframe::egui::{
    self,
    accesskit::{Action, Node, NodeId, Role},
};
use festerm_ios_window::{
    AccessibilityAdapter, ActionHandler, ActionRequest, ActivationHandler, DeactivationHandler,
    TreeUpdate,
};
use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

/// Platform clients cannot queue unbounded work between frames.
pub(crate) const MAX_PENDING_ACTIONS: usize = 32;

type Wake = Arc<dyn Fn() + Send + Sync>;

struct Shared {
    active: AtomicBool,
    actions: Mutex<VecDeque<ActionRequest>>,
    wake: Wake,
}

/// Activation, action and deactivation callbacks; called on the platform's
/// UI thread, so they only record state and wake the native loop.
#[derive(Clone)]
pub(crate) struct Handlers(Arc<Shared>);

impl ActivationHandler for Handlers {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        // The next frame produces a full tree; adapters accept it as the
        // initial tree when this returns `None`.
        self.0.active.store(true, Ordering::Release);
        (self.0.wake)();
        None
    }
}

impl ActionHandler for Handlers {
    fn do_action(&mut self, request: ActionRequest) {
        if !self.0.active.load(Ordering::Acquire) {
            return;
        }
        let Ok(mut actions) = self.0.actions.lock() else {
            return;
        };
        if actions.len() < MAX_PENDING_ACTIONS {
            actions.push_back(request);
        }
        drop(actions);
        (self.0.wake)();
    }
}

impl DeactivationHandler for Handlers {
    fn deactivate_accessibility(&mut self) {
        self.0.active.store(false, Ordering::Release);
        if let Ok(mut actions) = self.0.actions.lock() {
            actions.clear();
        }
        (self.0.wake)();
    }
}

thread_local! {
    // The native adapter is not `Send`; egui runs on the thread that created it.
    static ADAPTER: RefCell<Option<AccessibilityAdapter>> = const { RefCell::new(None) };
}

/// Installs the plugin; `adapter` is `None` off-device or when the native
/// adapter is unavailable, which leaves accessibility generation off.
pub(crate) fn install(
    ctx: &egui::Context,
    window: Option<raw_window_handle::WindowHandle<'_>>,
    wake: impl Fn() + Send + Sync + 'static,
) {
    let handlers = Handlers::new(wake);
    let adapter = window.and_then(|window| {
        AccessibilityAdapter::new(window, handlers.clone(), handlers.clone(), handlers.clone())
    });
    if adapter.is_none() {
        return;
    }
    ADAPTER.with(|slot| *slot.borrow_mut() = adapter);
    ctx.add_plugin(Plugin::new(handlers, publish));
}

fn publish(update: TreeUpdate) {
    ADAPTER.with(|slot| {
        if let Some(adapter) = slot.borrow_mut().as_mut() {
            adapter.update_if_active(|| update);
        }
    });
}

impl Handlers {
    pub(crate) fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Arc::new(Shared {
            active: AtomicBool::new(false),
            actions: Mutex::new(VecDeque::new()),
            wake: Arc::new(wake),
        }))
    }
}

pub(crate) struct Plugin {
    shared: Arc<Shared>,
    enabled: bool,
    publish: fn(TreeUpdate),
}

impl Plugin {
    pub(crate) fn new(handlers: Handlers, publish: fn(TreeUpdate)) -> Self {
        Self {
            shared: handlers.0,
            enabled: false,
            publish,
        }
    }
}

impl egui::Plugin for Plugin {
    fn debug_name(&self) -> &'static str {
        "festerm-accessibility"
    }

    fn input_hook(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        let active = self.shared.active.load(Ordering::Acquire);
        if active != self.enabled {
            self.enabled = active;
            if active {
                ctx.enable_accesskit();
            } else {
                ctx.disable_accesskit();
            }
        }
        if !active {
            return;
        }
        let Ok(mut actions) = self.shared.actions.lock() else {
            return;
        };
        input
            .events
            .extend(actions.drain(..).map(egui::Event::AccessKitActionRequest));
    }

    fn output_hook(&mut self, _ctx: &egui::Context, output: &mut egui::FullOutput) {
        if let Some(update) = output.platform_output.accesskit_update.take() {
            if self.enabled {
                let mut update = update;
                hoist_from_leaves(&mut update);
                (self.publish)(update);
            }
        }
    }
}

/// Actions that make UIKit treat a node as a leaf element and hide its
/// descendants (mirrors `accesskit_ios`'s element filter).
const LEAF_ACTIONS: [Action; 10] = [
    Action::Click,
    Action::Focus,
    Action::Increment,
    Action::Decrement,
    Action::Expand,
    Action::Collapse,
    Action::CustomAction,
    Action::ReplaceSelectedText,
    Action::SetTextSelection,
    Action::SetValue,
];

fn is_leaf_element(node: &Node) -> bool {
    LEAF_ACTIONS
        .iter()
        .any(|action| node.supports_action(*action))
        || node.toggled().is_some()
        || node.value().is_some()
        || node.numeric_value().is_some()
}

/// egui parents popups under their anchor button and may nest content under
/// other actionable widgets; iOS would hide that content. Moves such
/// children to follow their former parent; text runs stay with their field.
fn hoist_from_leaves(update: &mut TreeUpdate) {
    // Each pass lifts content one level; egui trees are shallow.
    for _ in 0..8 {
        let index: HashMap<NodeId, usize> = update
            .nodes
            .iter()
            .enumerate()
            .map(|(position, (id, _))| (*id, position))
            .collect();
        let mut parent_of = HashMap::new();
        for (id, node) in &update.nodes {
            for child in node.children() {
                parent_of.insert(*child, *id);
            }
        }
        let mut moved = false;
        for position in 0..update.nodes.len() {
            let (id, node) = &update.nodes[position];
            let id = *id;
            if !is_leaf_element(node) {
                continue;
            }
            let Some(&parent) = parent_of.get(&id) else {
                continue;
            };
            let (kept, lifted): (Vec<NodeId>, Vec<NodeId>) =
                node.children().iter().partition(|child| {
                    index
                        .get(child)
                        .is_some_and(|&at| update.nodes[at].1.role() == Role::TextRun)
                });
            if lifted.is_empty() {
                continue;
            }
            update.nodes[position].1.set_children(kept);
            let Some(&parent_at) = index.get(&parent) else {
                continue;
            };
            let parent_node = &mut update.nodes[parent_at].1;
            let mut siblings = parent_node.children().to_vec();
            let after = siblings
                .iter()
                .position(|sibling| *sibling == id)
                .map_or(siblings.len(), |at| at + 1);
            siblings.splice(after..after, lifted.iter().copied());
            parent_node.set_children(siblings);
            for child in lifted {
                parent_of.insert(child, parent);
            }
            moved = true;
        }
        if !moved {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::accesskit::{Action, NodeId, Role, TreeId};
    use std::sync::atomic::AtomicUsize;

    thread_local! {
        static PUBLISHED: RefCell<Vec<TreeUpdate>> = const { RefCell::new(Vec::new()) };
    }

    fn record(update: TreeUpdate) {
        PUBLISHED.with(|published| published.borrow_mut().push(update));
    }

    fn take_published() -> Vec<TreeUpdate> {
        PUBLISHED.with(|published| std::mem::take(&mut *published.borrow_mut()))
    }

    struct Fixture {
        ctx: egui::Context,
        handlers: Handlers,
        wakes: Arc<AtomicUsize>,
        clicks: usize,
    }

    impl Fixture {
        fn new() -> Self {
            let wakes = Arc::new(AtomicUsize::new(0));
            let counter = wakes.clone();
            let handlers = Handlers::new(move || {
                counter.fetch_add(1, Ordering::Relaxed);
            });
            let ctx = egui::Context::default();
            ctx.add_plugin(Plugin::new(handlers.clone(), record));
            take_published();
            Self {
                ctx,
                handlers,
                wakes,
                clicks: 0,
            }
        }

        fn frame(&mut self) {
            let mut clicks = 0;
            let mut output = self.ctx.run_ui(egui::RawInput::default(), |ui| {
                if ui.button("Connect").clicked() {
                    clicks += 1;
                }
            });
            output.textures_delta.clear();
            self.clicks += clicks;
        }

        fn connect_node(update: &TreeUpdate) -> NodeId {
            update
                .nodes
                .iter()
                .find(|(_, node)| node.role() == Role::Button && node.label() == Some("Connect"))
                .map(|(id, _)| *id)
                .expect("Connect button published")
        }

        fn click(&mut self, target_node: NodeId) {
            self.handlers.do_action(ActionRequest {
                action: Action::Click,
                target_tree: TreeId::ROOT,
                target_node,
                data: None,
            });
        }
    }

    #[test]
    fn tree_generation_waits_for_a_platform_client_and_stops_on_deactivation() {
        let mut fixture = Fixture::new();
        fixture.frame();
        assert!(take_published().is_empty());

        assert!(fixture.handlers.clone().request_initial_tree().is_none());
        assert_eq!(fixture.wakes.load(Ordering::Relaxed), 1);
        fixture.frame();
        let published = take_published();
        assert_eq!(published.len(), 1);
        assert!(published[0].tree.is_some(), "first update is a full tree");
        Fixture::connect_node(&published[0]);

        fixture.handlers.clone().deactivate_accessibility();
        fixture.frame();
        fixture.frame();
        assert!(take_published().is_empty());
    }

    #[test]
    fn platform_actions_reach_widgets_only_while_active_and_stay_bounded() {
        let mut fixture = Fixture::new();
        fixture.handlers.clone().request_initial_tree();
        fixture.frame();
        let connect = Fixture::connect_node(&take_published()[0]);

        fixture.click(connect);
        fixture.frame();
        assert_eq!(fixture.clicks, 1);

        for _ in 0..MAX_PENDING_ACTIONS + 8 {
            fixture.click(connect);
        }
        assert_eq!(
            fixture.handlers.0.actions.lock().unwrap().len(),
            MAX_PENDING_ACTIONS
        );

        // Deactivation discards queued work, and inactive clients cannot queue.
        fixture.handlers.clone().deactivate_accessibility();
        fixture.click(connect);
        fixture.frame();
        assert!(fixture.handlers.0.actions.lock().unwrap().is_empty());
        assert_eq!(fixture.clicks, 1);
    }

    #[test]
    fn content_under_actionable_widgets_is_lifted_so_ios_does_not_hide_it() {
        let fixture = Fixture::new();
        fixture.handlers.clone().request_initial_tree();
        let mut output = fixture.ctx.run_ui(egui::RawInput::default(), |ui| {
            let more = ui.button("More");
            egui::Popup::menu(&more)
                .open_memory(Some(egui::SetOpenCommand::Bool(true)))
                .show(|ui| {
                    let _ = ui.button("Reset demo");
                    ui.add(egui::TextEdit::singleline(&mut String::from("typed")));
                });
        });
        output.textures_delta.clear();
        let update = take_published().pop().expect("published tree");
        let nodes: HashMap<NodeId, &Node> = update.nodes.iter().map(|(id, n)| (*id, n)).collect();
        let find = |role: Role, label: &str| {
            update
                .nodes
                .iter()
                .find(|(_, n)| n.role() == role && n.label() == Some(label))
                .map(|(id, _)| *id)
                .unwrap()
        };
        let reset = find(Role::Button, "Reset demo");
        let mut ancestors = Vec::new();
        let mut cursor = reset;
        while let Some((parent, _)) = nodes.iter().find(|(_, n)| n.children().contains(&cursor)) {
            ancestors.push(*parent);
            cursor = *parent;
        }
        assert_eq!(
            cursor,
            update.tree.as_ref().unwrap().root,
            "still reachable"
        );
        assert!(
            ancestors.iter().all(|id| !is_leaf_element(nodes[id])),
            "Reset demo sits under a leaf element"
        );
        let (_, field) = update
            .nodes
            .iter()
            .find(|(_, n)| n.role() == Role::TextInput)
            .unwrap();
        assert!(field
            .children()
            .iter()
            .all(|child| nodes[child].role() == Role::TextRun));
        assert!(
            !field.children().is_empty(),
            "text runs stay with the field"
        );
    }
}
