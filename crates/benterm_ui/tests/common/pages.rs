//! Pages that show nothing and remember what a browser pane asked of
//! them, in place of the ones WebKit draws.

// Each test file uses its own part of this.
#![allow(dead_code)]

use std::cell::RefCell;
use std::rc::Rc;

use benterm_ui::browser::{Page, PageEdit, PageEvent, PageState, set_page_opener};
use futures::channel::mpsc::UnboundedSender;
use gpui::{App, Bounds, Pixels};

/// What a browser pane asked of its page, oldest first.
#[derive(Default)]
pub struct Asked {
    pub loads: Vec<String>,
    /// Where the page was put; `None` when it was hidden.
    pub places: Vec<Option<Bounds<Pixels>>>,
    /// Whether the page was given the keyboard, or had it taken away.
    pub keyboard: Vec<bool>,
    pub backs: usize,
    pub forwards: usize,
    pub reloads: usize,
    /// The commands of the Edit menu the page was asked to do.
    pub edits: Vec<PageEdit>,
    pub closed: bool,
    /// Whether the page took the keyboard itself, as when it is clicked.
    pub took_keyboard: bool,
    /// What the page says of itself.
    pub state: PageState,
    events: Option<UnboundedSender<PageEvent>>,
}

impl Asked {
    /// Where the page is now; `None` while it is hidden or was never put
    /// anywhere.
    pub fn place(&self) -> Option<Bounds<Pixels>> {
        self.places.last().copied().flatten()
    }

    /// Whether the page has the keyboard now.
    pub fn has_keyboard(&self) -> bool {
        self.took_keyboard || self.keyboard.last().copied().unwrap_or(false)
    }

    /// Takes the keyboard as a page that is clicked does, and says so.
    pub fn take_keyboard(&mut self) {
        self.took_keyboard = true;
        self.tell(PageEvent::TookKeyboard);
    }

    /// Tells the pane what WebKit would: that the page changed, or that
    /// it was clicked.
    pub fn tell(&self, event: PageEvent) {
        let events = self.events.as_ref().expect("the page was opened");
        events.unbounded_send(event).unwrap();
    }
}

struct FakePage(Rc<RefCell<Asked>>);

impl Page for FakePage {
    fn load(&mut self, url: &str) {
        let mut asked = self.0.borrow_mut();
        asked.loads.push(url.to_owned());
        asked.state.url = Some(url.to_owned());
    }

    fn go_back(&mut self) {
        self.0.borrow_mut().backs += 1;
    }

    fn go_forward(&mut self) {
        self.0.borrow_mut().forwards += 1;
    }

    fn reload(&mut self) {
        self.0.borrow_mut().reloads += 1;
    }

    fn state(&self) -> PageState {
        self.0.borrow().state.clone()
    }

    fn place(&mut self, place: Option<Bounds<Pixels>>) {
        let mut asked = self.0.borrow_mut();
        if place.is_none() {
            // A hidden page does not keep the keyboard.
            asked.took_keyboard = false;
        }
        asked.places.push(place);
    }

    fn set_keyboard(&mut self, on: bool) {
        let mut asked = self.0.borrow_mut();
        asked.took_keyboard = false;
        asked.keyboard.push(on);
    }

    fn has_keyboard(&self) -> bool {
        self.0.borrow().has_keyboard()
    }

    fn edit(&mut self, edit: PageEdit) {
        self.0.borrow_mut().edits.push(edit);
    }
}

impl Drop for FakePage {
    fn drop(&mut self) {
        self.0.borrow_mut().closed = true;
    }
}

/// The pages the app opened, oldest first.
#[derive(Clone, Default)]
pub struct Pages(Rc<RefCell<Vec<Rc<RefCell<Asked>>>>>);

impl Pages {
    /// Makes every browser pane opened from now on show a page that
    /// remembers what it is asked.
    pub fn install(cx: &mut App) -> Self {
        let pages = Self::default();
        let opened = pages.clone();
        set_page_opener(
            cx,
            Rc::new(move |_, events| {
                let asked = Rc::new(RefCell::new(Asked {
                    events: Some(events),
                    ..Asked::default()
                }));
                opened.0.borrow_mut().push(asked.clone());
                Box::new(FakePage(asked))
            }),
        );
        pages
    }

    pub fn len(&self) -> usize {
        self.0.borrow().len()
    }

    /// The page opened as number `index`, from 0.
    pub fn page(&self, index: usize) -> Rc<RefCell<Asked>> {
        self.0.borrow()[index].clone()
    }
}
