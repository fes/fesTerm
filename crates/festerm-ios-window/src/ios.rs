use block2::RcBlock;
use objc2::{
    define_class, msg_send,
    rc::{Retained, Weak},
    runtime::{AnyObject, ProtocolObject},
    sel, ClassType, DeclaredClass, MainThreadOnly, Message,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{
    MainThreadMarker, NSDictionary, NSNotification, NSNotificationCenter, NSObject,
    NSObjectProtocol, NSString, NSURL,
};
use objc2_foundation::{NSArray, NSAttributedStringKey, NSComparisonResult, NSInteger, NSRange};
use objc2_ui_kit::{
    NSWritingDirection, UITextInput, UITextInputDelegate, UITextInputStringTokenizer,
    UITextInputTokenizer, UITextLayoutDirection, UITextPosition, UITextRange, UITextSelectionRect,
    UITextStorageDirection,
};
use objc2_ui_kit::{
    UIApplication, UIDevice, UIInputViewAudioFeedback, UIKeyInput,
    UIKeyboardDidChangeFrameNotification, UIKeyboardWillChangeFrameNotification, UIPasteboard,
    UIResponder, UIScene, UISceneConnectionOptions, UISceneDelegate, UISceneSession,
    UITextAutocapitalizationType, UITextAutocorrectionType, UITextInputTraits,
    UITextSmartDashesType, UITextSmartQuotesType, UITextSpellCheckingType, UIView, UIWindow,
    UIWindowScene, UIWindowSceneDelegate,
};
use raw_window_handle::{RawWindowHandle, WindowHandle};
use std::{
    cell::{Cell, OnceCell, RefCell},
    ops::Range,
    ptr::NonNull,
    sync::Arc,
};

use crate::{FieldDocument, FieldInput, FieldModel, FieldSync, KeyboardMode};

type Repaint = Arc<dyn Fn() + Send + Sync>;

/// Main-thread-only: owns a retain on the host view and removes observers on
/// drop. Notification callbacks capture only a Send + Sync repaint closure.
pub struct KeyboardBridge {
    view: Retained<UIView>,
    center: Retained<NSNotificationCenter>,
    observers: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
    terminal_responder: Retained<TerminalResponder>,
    field_responder: Retained<FieldResponder>,
    repaint: Repaint,
}

struct TerminalResponderIvars {
    target: Weak<UIView>,
    empty_input: Retained<TerminalInputView>,
}

define_class!(
    #[unsafe(super(UIView))]
    #[thread_kind = MainThreadOnly]
    #[name = "FesTermTerminalInputView"]
    struct TerminalInputView;

    unsafe impl NSObjectProtocol for TerminalInputView {}

    unsafe impl UIInputViewAudioFeedback for TerminalInputView {
        #[unsafe(method(enableInputClicksWhenVisible))]
        fn enable_input_clicks_when_visible(&self) -> bool {
            true
        }
    }
);

define_class!(
    /// Keep UIKit's hardware text delivery without showing a second software
    /// keyboard. Forward to winit's existing UIKeyInput rather than storing text.
    #[unsafe(super(UIView))]
    #[thread_kind = MainThreadOnly]
    #[name = "FesTermTerminalKeyboardResponder"]
    #[ivars = TerminalResponderIvars]
    struct TerminalResponder;

    unsafe impl NSObjectProtocol for TerminalResponder {}

    impl TerminalResponder {
        #[unsafe(method(canBecomeFirstResponder))]
        fn can_become_first_responder(&self) -> bool { true }

        #[unsafe(method(inputView))]
        fn input_view(&self) -> &UIView {
            &self.ivars().empty_input
        }
    }

    unsafe impl UITextInputTraits for TerminalResponder {
        #[unsafe(method(autocapitalizationType))]
        fn autocapitalization_type(&self) -> UITextAutocapitalizationType {
            UITextAutocapitalizationType::None
        }
        #[unsafe(method(autocorrectionType))]
        fn autocorrection_type(&self) -> UITextAutocorrectionType {
            UITextAutocorrectionType::No
        }
        #[unsafe(method(spellCheckingType))]
        fn spell_checking_type(&self) -> UITextSpellCheckingType {
            UITextSpellCheckingType::No
        }
        #[unsafe(method(smartQuotesType))]
        fn smart_quotes_type(&self) -> UITextSmartQuotesType {
            UITextSmartQuotesType::No
        }
        #[unsafe(method(smartDashesType))]
        fn smart_dashes_type(&self) -> UITextSmartDashesType {
            UITextSmartDashesType::No
        }
    }

    unsafe impl UIKeyInput for TerminalResponder {
        #[unsafe(method(hasText))]
        fn has_text(&self) -> bool { true }

        #[unsafe(method(insertText:))]
        fn insert_text(&self, text: &NSString) {
            if let Some(target) = self.ivars().target.load() {
                // SAFETY: selectors are checked when the bridge is constructed;
                // the retained winit view is accessed only on the main thread.
                let _: () = unsafe { msg_send![&*target, insertText: text] };
            }
        }

        #[unsafe(method(deleteBackward))]
        fn delete_backward(&self) {
            if let Some(target) = self.ivars().target.load() {
                // SAFETY: same checked winit UIKeyInput receiver as insertText:.
                let _: () = unsafe { msg_send![&*target, deleteBackward] };
            }
        }
    }
);

struct FieldPositionIvars {
    offset: usize,
}

define_class!(
    /// A char offset into the focused field's [`FieldModel`].
    #[unsafe(super(UITextPosition))]
    #[thread_kind = MainThreadOnly]
    #[name = "FesTermFieldPosition"]
    #[ivars = FieldPositionIvars]
    struct FieldPosition;

    unsafe impl NSObjectProtocol for FieldPosition {}
);

impl FieldPosition {
    fn create(mtm: MainThreadMarker, offset: usize) -> Retained<UITextPosition> {
        let this = Self::alloc(mtm).set_ivars(FieldPositionIvars { offset });
        // SAFETY: NSObject initializer on a newly allocated main-thread object.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        Retained::into_super(this)
    }
}

struct FieldRangeIvars {
    start: usize,
    end: usize,
}

define_class!(
    #[unsafe(super(UITextRange))]
    #[thread_kind = MainThreadOnly]
    #[name = "FesTermFieldRange"]
    #[ivars = FieldRangeIvars]
    struct FieldRange;

    unsafe impl NSObjectProtocol for FieldRange {}

    impl FieldRange {
        #[unsafe(method(isEmpty))]
        fn is_empty(&self) -> bool {
            self.ivars().start == self.ivars().end
        }

        #[unsafe(method_id(start))]
        fn start(&self) -> Retained<UITextPosition> {
            FieldPosition::create(MainThreadMarker::from(self), self.ivars().start)
        }

        #[unsafe(method_id(end))]
        fn end(&self) -> Retained<UITextPosition> {
            FieldPosition::create(MainThreadMarker::from(self), self.ivars().end)
        }
    }
);

impl FieldRange {
    fn create(mtm: MainThreadMarker, range: Range<usize>) -> Retained<UITextRange> {
        let this = Self::alloc(mtm).set_ivars(FieldRangeIvars {
            start: range.start,
            end: range.end.max(range.start),
        });
        // SAFETY: NSObject initializer on a newly allocated main-thread object.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        Retained::into_super(this)
    }
}

fn offset(position: &UITextPosition) -> usize {
    position
        .downcast_ref::<FieldPosition>()
        .map_or(0, |position| position.ivars().offset)
}

fn range(range: &UITextRange) -> Range<usize> {
    range
        .downcast_ref::<FieldRange>()
        .map_or(0..0, |range| range.ivars().start..range.ivars().end)
}

/// Converts a UTF-16 range within `text` to chars.
fn char_range(text: &str, utf16: NSRange) -> Range<usize> {
    let mut units = 0;
    let mut start = None;
    let mut end = None;
    for (index, ch) in text.chars().chain(std::iter::once('\0')).enumerate() {
        if start.is_none() && units >= utf16.location {
            start = Some(index);
        }
        if end.is_none() && units >= utf16.location + utf16.length {
            end = Some(index);
            break;
        }
        units += ch.len_utf16();
    }
    let len = text.chars().count();
    let start = start.unwrap_or(len);
    start..end.unwrap_or(len).max(start)
}

struct FieldResponderIvars {
    target: Weak<UIView>,
    empty_input: Retained<TerminalInputView>,
    custom_input: Cell<bool>,
    model: RefCell<FieldModel>,
    delegate: RefCell<Option<Weak<ProtocolObject<dyn UITextInputDelegate>>>>,
    tokenizer: OnceCell<Retained<UITextInputStringTokenizer>>,
    repaint: Repaint,
}

define_class!(
    /// The system keyboard's text-aware client for egui form fields: it
    /// answers `UITextInput` from the app-published [`FieldModel`], so iOS
    /// offers the space-bar trackpad and marked text, and queues edits back.
    #[unsafe(super(UIView))]
    #[thread_kind = MainThreadOnly]
    #[name = "FesTermFieldResponder"]
    #[ivars = FieldResponderIvars]
    struct FieldResponder;

    unsafe impl NSObjectProtocol for FieldResponder {}

    impl FieldResponder {
        #[unsafe(method(canBecomeFirstResponder))]
        fn can_become_first_responder(&self) -> bool { true }

        #[unsafe(method_id(inputView))]
        fn input_view(&self) -> Option<Retained<UIView>> {
            self.ivars().custom_input.get().then(|| {
                Retained::into_super(self.ivars().empty_input.clone())
            })
        }

        #[unsafe(method(beginFloatingCursorAtPoint:))]
        fn begin_floating_cursor(&self, point: CGPoint) {
            self.move_caret_to(point);
        }

        #[unsafe(method(updateFloatingCursorAtPoint:))]
        fn update_floating_cursor(&self, point: CGPoint) {
            self.move_caret_to(point);
        }

        #[unsafe(method(endFloatingCursor))]
        fn end_floating_cursor(&self) {}

        #[unsafe(method_id(textInputView))]
        fn text_input_view(&self) -> Retained<UIView> {
            self.ivars()
                .target
                .load()
                .unwrap_or_else(|| Retained::into_super(self.retain()))
        }

        #[unsafe(method(selectionAffinity))]
        fn selection_affinity(&self) -> UITextStorageDirection {
            UITextStorageDirection::Forward
        }
    }

    unsafe impl UITextInputTraits for FieldResponder {
        #[unsafe(method(autocapitalizationType))]
        fn autocapitalization_type(&self) -> UITextAutocapitalizationType {
            UITextAutocapitalizationType::None
        }
        #[unsafe(method(autocorrectionType))]
        fn autocorrection_type(&self) -> UITextAutocorrectionType {
            UITextAutocorrectionType::No
        }
        #[unsafe(method(spellCheckingType))]
        fn spell_checking_type(&self) -> UITextSpellCheckingType {
            UITextSpellCheckingType::No
        }
        #[unsafe(method(smartQuotesType))]
        fn smart_quotes_type(&self) -> UITextSmartQuotesType {
            UITextSmartQuotesType::No
        }
        #[unsafe(method(smartDashesType))]
        fn smart_dashes_type(&self) -> UITextSmartDashesType {
            UITextSmartDashesType::No
        }
        #[unsafe(method(isSecureTextEntry))]
        fn is_secure_text_entry(&self) -> bool {
            self.ivars().model.borrow().secure()
        }
    }

    unsafe impl UIKeyInput for FieldResponder {
        #[unsafe(method(hasText))]
        fn has_text(&self) -> bool {
            !self.ivars().model.borrow().is_empty()
        }

        #[unsafe(method(insertText:))]
        fn insert_text(&self, text: &NSString) {
            self.edit(|model| model.insert(&text.to_string()));
        }

        #[unsafe(method(deleteBackward))]
        fn delete_backward(&self) {
            self.edit(FieldModel::delete_backward);
        }
    }

    unsafe impl UITextInput for FieldResponder {
        #[unsafe(method_id(textInRange:))]
        fn text_in_range(&self, text_range: &UITextRange) -> Option<Retained<NSString>> {
            let model = self.ivars().model.borrow();
            let range = model.clamp(range(text_range));
            Some(NSString::from_str(&if model.secure() {
                "\u{2022}".repeat(range.len())
            } else {
                model.text(range)
            }))
        }

        #[unsafe(method(replaceRange:withText:))]
        fn replace_range(&self, text_range: &UITextRange, text: &NSString) {
            let range = range(text_range);
            self.edit(|model| model.replace(range, &text.to_string()));
        }

        #[unsafe(method_id(selectedTextRange))]
        fn selected_text_range(&self) -> Option<Retained<UITextRange>> {
            let selection = self.ivars().model.borrow().selection();
            Some(FieldRange::create(MainThreadMarker::from(self), selection))
        }

        #[unsafe(method(setSelectedTextRange:))]
        fn set_selected_text_range(&self, text_range: Option<&UITextRange>) {
            if let Some(text_range) = text_range {
                let range = range(text_range);
                self.edit(|model| model.set_selection(range));
            }
        }

        #[unsafe(method_id(markedTextRange))]
        fn marked_text_range(&self) -> Option<Retained<UITextRange>> {
            let marked = self.ivars().model.borrow().marked();
            marked.map(|marked| FieldRange::create(MainThreadMarker::from(self), marked))
        }

        #[unsafe(method_id(markedTextStyle))]
        fn marked_text_style(
            &self,
        ) -> Option<Retained<NSDictionary<NSAttributedStringKey, AnyObject>>> {
            None
        }

        #[unsafe(method(setMarkedTextStyle:))]
        fn set_marked_text_style(
            &self,
            _style: Option<&NSDictionary<NSAttributedStringKey, AnyObject>>,
        ) {
        }

        #[unsafe(method(setMarkedText:selectedRange:))]
        fn set_marked_text(&self, text: Option<&NSString>, selected: NSRange) {
            let text = text.map(NSString::to_string).unwrap_or_default();
            let selected = char_range(&text, selected);
            self.edit(|model| model.set_marked(&text, selected));
        }

        #[unsafe(method(unmarkText))]
        fn unmark_text(&self) {
            self.edit(FieldModel::unmark);
        }

        #[unsafe(method_id(beginningOfDocument))]
        fn beginning_of_document(&self) -> Retained<UITextPosition> {
            FieldPosition::create(MainThreadMarker::from(self), 0)
        }

        #[unsafe(method_id(endOfDocument))]
        fn end_of_document(&self) -> Retained<UITextPosition> {
            let len = self.ivars().model.borrow().len();
            FieldPosition::create(MainThreadMarker::from(self), len)
        }

        #[unsafe(method_id(textRangeFromPosition:toPosition:))]
        fn text_range_from_position(
            &self,
            from: &UITextPosition,
            to: &UITextPosition,
        ) -> Option<Retained<UITextRange>> {
            let (from, to) = (offset(from), offset(to));
            Some(FieldRange::create(
                MainThreadMarker::from(self),
                from.min(to)..from.max(to),
            ))
        }

        #[unsafe(method_id(positionFromPosition:offset:))]
        fn position_from_position(
            &self,
            position: &UITextPosition,
            by: NSInteger,
        ) -> Option<Retained<UITextPosition>> {
            let len = self.ivars().model.borrow().len();
            offset(position)
                .checked_add_signed(by)
                .filter(|target| *target <= len)
                .map(|target| FieldPosition::create(MainThreadMarker::from(self), target))
        }

        #[unsafe(method_id(positionFromPosition:inDirection:offset:))]
        fn position_from_position_in_direction(
            &self,
            position: &UITextPosition,
            direction: UITextLayoutDirection,
            by: NSInteger,
        ) -> Option<Retained<UITextPosition>> {
            let model = self.ivars().model.borrow();
            let len = model.len();
            let from = offset(position);
            let target = match direction {
                UITextLayoutDirection::Left => from.saturating_add_signed(by.saturating_neg()).min(len),
                UITextLayoutDirection::Right => from.saturating_add_signed(by).min(len),
                UITextLayoutDirection::Up => model.vertical(from, by.saturating_neg()),
                UITextLayoutDirection::Down => model.vertical(from, by),
                _ => from.min(len),
            };
            Some(FieldPosition::create(MainThreadMarker::from(self), target))
        }

        #[unsafe(method(comparePosition:toPosition:))]
        fn compare_position(
            &self,
            position: &UITextPosition,
            other: &UITextPosition,
        ) -> NSComparisonResult {
            match offset(position).cmp(&offset(other)) {
                std::cmp::Ordering::Less => NSComparisonResult::Ascending,
                std::cmp::Ordering::Equal => NSComparisonResult::Same,
                std::cmp::Ordering::Greater => NSComparisonResult::Descending,
            }
        }

        #[unsafe(method(offsetFromPosition:toPosition:))]
        fn offset_from_position(&self, from: &UITextPosition, to: &UITextPosition) -> NSInteger {
            offset(to) as NSInteger - offset(from) as NSInteger
        }

        #[unsafe(method_id(inputDelegate))]
        fn input_delegate(&self) -> Option<Retained<ProtocolObject<dyn UITextInputDelegate>>> {
            self.ivars().delegate.borrow().as_ref().and_then(Weak::load)
        }

        #[unsafe(method(setInputDelegate:))]
        fn set_input_delegate(&self, delegate: Option<&ProtocolObject<dyn UITextInputDelegate>>) {
            *self.ivars().delegate.borrow_mut() = delegate.map(Weak::new);
        }

        #[unsafe(method_id(tokenizer))]
        fn tokenizer(&self) -> Retained<ProtocolObject<dyn UITextInputTokenizer>> {
            let tokenizer = self.ivars().tokenizer.get_or_init(|| {
                let responder: &UIResponder = self;
                // SAFETY: this responder implements UITextInput; the
                // tokenizer is created and used on the main thread.
                unsafe {
                    UITextInputStringTokenizer::initWithTextInput(
                        UITextInputStringTokenizer::alloc(MainThreadMarker::from(self)),
                        responder,
                    )
                }
            });
            ProtocolObject::from_retained(tokenizer.clone())
        }

        #[unsafe(method_id(positionWithinRange:farthestInDirection:))]
        fn position_within_range(
            &self,
            text_range: &UITextRange,
            direction: UITextLayoutDirection,
        ) -> Option<Retained<UITextPosition>> {
            let range = range(text_range);
            let target = match direction {
                UITextLayoutDirection::Left | UITextLayoutDirection::Up => range.start,
                _ => range.end,
            };
            Some(FieldPosition::create(MainThreadMarker::from(self), target))
        }

        #[unsafe(method_id(characterRangeByExtendingPosition:inDirection:))]
        fn character_range_by_extending(
            &self,
            position: &UITextPosition,
            direction: UITextLayoutDirection,
        ) -> Option<Retained<UITextRange>> {
            let len = self.ivars().model.borrow().len();
            let at = offset(position).min(len);
            let range = match direction {
                UITextLayoutDirection::Left | UITextLayoutDirection::Up => {
                    at.saturating_sub(1)..at
                }
                _ => at..(at + 1).min(len),
            };
            Some(FieldRange::create(MainThreadMarker::from(self), range))
        }

        #[unsafe(method(baseWritingDirectionForPosition:inDirection:))]
        fn base_writing_direction(
            &self,
            _position: &UITextPosition,
            _direction: UITextStorageDirection,
        ) -> NSWritingDirection {
            NSWritingDirection::LeftToRight
        }

        #[unsafe(method(setBaseWritingDirection:forRange:))]
        fn set_base_writing_direction(
            &self,
            _direction: NSWritingDirection,
            _range: &UITextRange,
        ) {
        }

        #[unsafe(method(firstRectForRange:))]
        fn first_rect_for_range(&self, text_range: &UITextRange) -> CGRect {
            let model = self.ivars().model.borrow();
            let range = range(text_range);
            let (top, bottom) = model.row_at(range.start);
            let start = model.caret_x(range.start);
            let end = (range.start..=range.end.min(model.len()))
                .take_while(|offset| model.row_at(*offset) == (top, bottom))
                .map(|offset| model.caret_x(offset))
                .fold(start, f32::max);
            CGRect::new(
                CGPoint::new(start.min(end).into(), top.into()),
                CGSize::new((end - start).abs().into(), (bottom - top).into()),
            )
        }

        #[unsafe(method(caretRectForPosition:))]
        fn caret_rect_for_position(&self, position: &UITextPosition) -> CGRect {
            let model = self.ivars().model.borrow();
            let (top, bottom) = model.row_at(offset(position));
            CGRect::new(
                CGPoint::new(model.caret_x(offset(position)).into(), top.into()),
                CGSize::new(2.0, (bottom - top).into()),
            )
        }

        #[unsafe(method_id(selectionRectsForRange:))]
        fn selection_rects_for_range(
            &self,
            _range: &UITextRange,
        ) -> Retained<NSArray<UITextSelectionRect>> {
            NSArray::new()
        }

        #[unsafe(method_id(closestPositionToPoint:))]
        fn closest_position_to_point(&self, point: CGPoint) -> Option<Retained<UITextPosition>> {
            let closest = self.ivars().model.borrow().closest_at(point.x as f32, point.y as f32);
            Some(FieldPosition::create(MainThreadMarker::from(self), closest))
        }

        #[unsafe(method_id(closestPositionToPoint:withinRange:))]
        fn closest_position_within_range(
            &self,
            point: CGPoint,
            text_range: &UITextRange,
        ) -> Option<Retained<UITextPosition>> {
            let range = range(text_range);
            let closest = self.ivars().model.borrow().closest_at(point.x as f32, point.y as f32);
            Some(FieldPosition::create(
                MainThreadMarker::from(self),
                closest.clamp(range.start, range.end),
            ))
        }

        #[unsafe(method_id(characterRangeAtPoint:))]
        fn character_range_at_point(&self, point: CGPoint) -> Option<Retained<UITextRange>> {
            let model = self.ivars().model.borrow();
            let at = model.closest_at(point.x as f32, point.y as f32);
            let range = model.clamp(at..at + 1);
            Some(FieldRange::create(MainThreadMarker::from(self), range))
        }
    }
);

impl FieldResponder {
    fn edit(&self, change: impl FnOnce(&mut FieldModel)) {
        if !self.ivars().model.borrow().is_active() {
            return;
        }
        change(&mut self.ivars().model.borrow_mut());
        (self.ivars().repaint)();
    }

    fn move_caret_to(&self, point: CGPoint) {
        let closest = self
            .ivars()
            .model
            .borrow()
            .closest_at(point.x as f32, point.y as f32);
        self.edit(|model| model.set_selection(closest..closest));
    }

    fn sync(&self, document: Option<FieldDocument>) {
        let was_secure = self.ivars().model.borrow().secure();
        let change = self.ivars().model.borrow_mut().sync(document);
        if change == FieldSync::Unchanged {
            return;
        }
        if self.ivars().model.borrow().secure() != was_secure && self.isFirstResponder() {
            self.reloadInputViews();
        }
        let Some(delegate) = self.ivars().delegate.borrow().as_ref().and_then(Weak::load) else {
            return;
        };
        let input = ProtocolObject::<dyn UITextInput>::from_ref(self);
        // The model is not borrowed here: the delegate re-enters UITextInput.
        if change == FieldSync::Text {
            delegate.textWillChange(Some(input));
            delegate.textDidChange(Some(input));
        } else {
            delegate.selectionWillChange(Some(input));
            delegate.selectionDidChange(Some(input));
        }
    }

    fn take_input(&self) -> Vec<FieldInput> {
        let mut model = self.ivars().model.borrow_mut();
        let batch = model.take_batch();
        if model.has_pending() {
            (self.ivars().repaint)();
        }
        batch
    }
}

impl KeyboardBridge {
    pub fn new(
        window: WindowHandle<'_>,
        repaint: impl Fn() + Send + Sync + 'static,
    ) -> Option<Self> {
        let main_thread = MainThreadMarker::new()?;
        let RawWindowHandle::UiKit(handle) = window.as_raw() else {
            return None;
        };
        // SAFETY: WindowHandle guarantees a valid UIKit UIView pointer for
        // this call; retain it before the handle borrow ends. UIKit access
        // is confined to this main-thread-only type (UIView is !Send/!Sync).
        let view = unsafe { Retained::retain(handle.ui_view.as_ptr().cast::<UIView>()) }?;
        if !view.respondsToSelector(sel!(insertText:))
            || !view.respondsToSelector(sel!(deleteBackward))
        {
            eprintln!("festerm-mobile: host view has no keyboard input receiver");
            return None;
        }
        let input = TerminalInputView::alloc(main_thread).set_ivars(());
        // SAFETY: UIView initializer on a newly allocated main-thread object.
        let empty_input: Retained<TerminalInputView> = unsafe { msg_send![super(input), init] };
        let responder = TerminalResponder::alloc(main_thread).set_ivars(TerminalResponderIvars {
            target: Weak::from_retained(&view),
            empty_input,
        });
        // SAFETY: UIView initializer on a newly allocated main-thread object.
        let terminal_responder: Retained<TerminalResponder> =
            unsafe { msg_send![super(responder), init] };
        view.addSubview(&terminal_responder);
        let repaint: Repaint = Arc::new(repaint);
        let field = FieldResponder::alloc(main_thread).set_ivars(FieldResponderIvars {
            target: Weak::from_retained(&view),
            empty_input: terminal_responder.ivars().empty_input.clone(),
            custom_input: Cell::new(false),
            model: RefCell::default(),
            delegate: RefCell::default(),
            tokenizer: OnceCell::new(),
            repaint: repaint.clone(),
        });
        // SAFETY: UIView initializer on a newly allocated main-thread object.
        let field_responder: Retained<FieldResponder> = unsafe { msg_send![super(field), init] };
        view.addSubview(&field_responder);
        // Create the guide before the first keyboard animation/layout pass.
        let _ = view.keyboardLayoutGuide();
        let center = NSNotificationCenter::defaultCenter();
        let mut observers = Vec::with_capacity(2);
        // SAFETY: these UIKit notification names are immutable system globals.
        for name in unsafe {
            [
                UIKeyboardWillChangeFrameNotification,
                UIKeyboardDidChangeFrameNotification,
            ]
        } {
            let repaint = repaint.clone();
            let callback = RcBlock::new(move |_notification: NonNull<NSNotification>| repaint());
            // SAFETY: no object filter/queue, and the callback is Send + Sync,
            // captures no UIKit values, and never accesses notification data.
            observers.push(unsafe {
                center.addObserverForName_object_queue_usingBlock(Some(name), None, None, &callback)
            });
        }
        Some(Self {
            view,
            center,
            observers,
            terminal_responder,
            field_responder,
            repaint,
        })
    }

    pub fn document_picker(&self) -> Option<crate::DocumentPicker> {
        Some(crate::DocumentPicker::new(
            self.view.clone(),
            self.repaint.clone(),
        ))
    }

    /// Docked keyboard occlusion in view-relative units. Hidden/hardware or
    /// floating keyboards do not reserve a fabricated fixed-height region.
    pub fn occluded_height_fraction(&self) -> f32 {
        let bounds = self.view.bounds();
        let frame = self.view.keyboardLayoutGuide().layoutFrame();
        let safe_bottom = self.view.safeAreaInsets().bottom;
        if bounds.size.height <= 0.0 || frame.size.height <= safe_bottom + 1.0 {
            return 0.0;
        }
        ((bounds.origin.y + bounds.size.height - frame.origin.y) / bounds.size.height)
            .clamp(0.0, 1.0) as f32
    }

    pub fn set_keyboard_mode(&self, mode: KeyboardMode) -> Result<(), &'static str> {
        let field_mode = matches!(mode, KeyboardMode::Field | KeyboardMode::FieldCustom);
        if !field_mode
            && self.field_responder.isFirstResponder()
            && !self.field_responder.resignFirstResponder()
        {
            return Err("The field keyboard could not release input");
        }
        match mode {
            KeyboardMode::Terminal => {
                if self.view.isFirstResponder() && !self.view.resignFirstResponder() {
                    return Err("The built-in keyboard could not be dismissed");
                }
                if !self.terminal_responder.isFirstResponder() {
                    if !self.terminal_responder.becomeFirstResponder() {
                        return Err(
                            "Hardware keyboard input is unavailable; switch to the built-in keyboard",
                        );
                    }
                    self.terminal_responder.reloadInputViews();
                }
            }
            KeyboardMode::Field | KeyboardMode::FieldCustom => {
                let changed = self
                    .field_responder
                    .ivars()
                    .custom_input
                    .replace(mode == KeyboardMode::FieldCustom)
                    != (mode == KeyboardMode::FieldCustom);
                if !self.field_responder.isFirstResponder()
                    && !self.field_responder.becomeFirstResponder()
                {
                    return Err("The built-in keyboard is unavailable");
                }
                if changed {
                    self.field_responder.reloadInputViews();
                }
            }
            KeyboardMode::System | KeyboardMode::Hidden => {
                if self.terminal_responder.isFirstResponder()
                    && !self.terminal_responder.resignFirstResponder()
                {
                    return Err("The terminal keyboard could not release input");
                }
                if mode == KeyboardMode::System {
                    if !self.view.isFirstResponder() && !self.view.becomeFirstResponder() {
                        return Err("The built-in keyboard is unavailable");
                    }
                } else if self.view.isFirstResponder() && !self.view.resignFirstResponder() {
                    return Err("The built-in keyboard could not be dismissed");
                }
            }
        }
        Ok(())
    }

    /// Publishes the focused form field (or none) for the keyboard to edit.
    pub fn set_field_document(&self, document: Option<FieldDocument>) {
        self.field_responder.sync(document);
    }

    /// Keyboard edits for the published field, to apply before egui runs it.
    pub fn take_field_input(&self) -> Vec<FieldInput> {
        self.field_responder.take_input()
    }

    pub fn play_input_click(&self) {
        if self.terminal_responder.isFirstResponder()
            || (self.field_responder.isFirstResponder()
                && self.field_responder.ivars().custom_input.get())
        {
            UIDevice::currentDevice(MainThreadMarker::from(&*self.terminal_responder))
                .playInputClick();
        }
    }
}

impl Drop for KeyboardBridge {
    fn drop(&mut self) {
        self.terminal_responder.resignFirstResponder();
        self.terminal_responder.removeFromSuperview();
        self.field_responder.resignFirstResponder();
        self.field_responder.removeFromSuperview();
        for observer in &self.observers {
            // SAFETY: each token was returned by this center and is still
            // retained; removal prevents callbacks after the bridge is gone.
            unsafe { self.center.removeObserver((**observer).as_ref()) };
        }
    }
}

/// The general pasteboard's text. Main thread only; `None` elsewhere or when
/// the pasteboard holds no string.
pub fn clipboard_text() -> Option<String> {
    let _main_thread = MainThreadMarker::new()?;
    let pasteboard = UIPasteboard::generalPasteboard();
    // SAFETY: plain property read on a valid pasteboard.
    unsafe { pasteboard.string() }.map(|text| text.to_string())
}

/// Replaces the general pasteboard's text. Main thread only.
pub fn set_clipboard_text(text: &str) -> bool {
    let Some(_main_thread) = MainThreadMarker::new() else {
        return false;
    };
    let pasteboard = UIPasteboard::generalPasteboard();
    let text = NSString::from_str(text);
    // SAFETY: plain property write with a valid NSString.
    unsafe { pasteboard.setString(Some(&text)) };
    true
}

/// Opens an `https://` URL in the system browser. Main thread only; other
/// schemes are refused so a hostile string cannot launch another app.
pub fn open_url(url: &str) -> bool {
    let Some(main_thread) = MainThreadMarker::new() else {
        return false;
    };
    if !url.starts_with("https://") {
        return false;
    }
    let Some(url) = NSURL::URLWithString(&NSString::from_str(url)) else {
        return false;
    };
    let application = UIApplication::sharedApplication(main_thread);
    // SAFETY: valid URL and an empty options dictionary; no completion block.
    unsafe {
        application.openURL_options_completionHandler(&url, &NSDictionary::new(), None);
    }
    true
}

/// Class name referenced by `UISceneDelegateClassName` in the iOS Info.plist.
pub const SCENE_DELEGATE_CLASS: &str = "FesTermSceneDelegate";

define_class!(
    /// Minimal UIScene adoption (required from iOS 27). winit 0.30 creates its
    /// window in `didFinishLaunching`, before the scene connects, so the
    /// delegate attaches any scene-less window when the scene arrives.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FesTermSceneDelegate"]
    struct SceneDelegate;

    unsafe impl NSObjectProtocol for SceneDelegate {}

    unsafe impl UISceneDelegate for SceneDelegate {
        #[unsafe(method(scene:willConnectToSession:options:))]
        fn scene_will_connect(
            &self,
            scene: &UIScene,
            _session: &UISceneSession,
            _options: &UISceneConnectionOptions,
        ) {
            let mtm = MainThreadMarker::from(self);
            if let Some(window_scene) = scene.downcast_ref::<UIWindowScene>() {
                CONNECTED_SCENE.set(Some(window_scene.retain()));
                attach_pending(mtm);
            }
        }
    }

    unsafe impl UIWindowSceneDelegate for SceneDelegate {}
);

thread_local! {
    static CONNECTED_SCENE: Cell<Option<Retained<UIWindowScene>>> = const { Cell::new(None) };
    static APP_WINDOW: Cell<Option<Retained<UIWindow>>> = const { Cell::new(None) };
}

/// Hands winit's window (the one hosting `window`'s view) to the scene
/// delegate. iOS 27 omits scene-less windows from `UIApplication.windows`, so
/// the window is taken from the view rather than searched for. Main thread
/// only; attaches immediately if the scene already connected.
pub fn attach_window(window: WindowHandle<'_>) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let RawWindowHandle::UiKit(handle) = window.as_raw() else {
        return;
    };
    // SAFETY: WindowHandle guarantees a valid UIKit UIView pointer for this
    // call; it is retained before the borrow ends, on the main thread.
    let Some(view) = (unsafe { Retained::retain(handle.ui_view.as_ptr().cast::<UIView>()) }) else {
        return;
    };
    let Some(window) = view.window() else {
        eprintln!("festerm-mobile: renderer view has no window");
        return;
    };
    APP_WINDOW.set(Some(window));
    attach_pending(mtm);
}

fn attach_pending(_mtm: MainThreadMarker) {
    let scene = CONNECTED_SCENE.take();
    let window = APP_WINDOW.take();
    if let (Some(scene), Some(window)) = (&scene, &window) {
        if window.windowScene().is_none() {
            window.setWindowScene(Some(scene));
            window.makeKeyAndVisible();
            eprintln!("festerm-mobile: attached the app window to the scene");
        }
    }
    CONNECTED_SCENE.set(scene);
    APP_WINDOW.set(window);
}

/// Registers the scene delegate class with the Objective-C runtime. Call
/// before the event loop starts `UIApplicationMain`; main thread only.
pub fn register_scene_delegate() -> bool {
    let _ = SceneDelegate::class();
    true
}
