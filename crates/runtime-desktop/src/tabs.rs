//! Plugin tabs on the Sunbird settings page (`phoenix.ui.addSettingsTab`).
//!
//! The page is drawn on Discord's render thread, so it reads the blocks each
//! tab's `render` returned last and never calls into the plugin.

use std::sync::{
   LazyLock,
   Mutex,
   PoisonError,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Variant {
   Primary,
   Secondary,
   Danger,
}

/// A button, with the id of the `onPress` the plugin registered for it.
#[derive(Clone)]
pub struct Button {
   pub label:   String,
   pub variant: Variant,
   pub id:      u64,
}

#[derive(Clone)]
pub enum Block {
   Text {
      title: String,
      text:  String,
   },
   /// A row with a title and description, and its button on the right.
   Action {
      title:       String,
      description: String,
      button:      Button,
   },
   Buttons(Vec<Button>),
   Divider,
}

#[derive(Clone)]
pub struct Tab {
   pub id:     u64,
   pub plugin: String,
   pub title:  String,
   pub blocks: Vec<Block>,
}

static TABS: LazyLock<Mutex<Vec<Tab>>> = LazyLock::new(|| Mutex::new(Vec::new()));

fn tabs() -> std::sync::MutexGuard<'static, Vec<Tab>> {
   TABS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Every tab's id, plugin and title, in the order they were added.
pub fn titles() -> Vec<(u64, String, String)> {
   tabs()
      .iter()
      .map(|tab| (tab.id, tab.plugin.clone(), tab.title.clone()))
      .collect()
}

pub fn get(id: u64) -> Option<Tab> {
   tabs().iter().find(|tab| tab.id == id).cloned()
}

pub fn add(tab: Tab) {
   tabs().push(tab);
}

/// Replaces `id`'s blocks. False when the tab is gone.
pub fn set_blocks(id: u64, blocks: Vec<Block>) -> bool {
   if let Some(tab) = tabs().iter_mut().find(|tab| tab.id == id) {
      tab.blocks = blocks;
      return true;
   }
   false
}

pub fn remove(id: u64) {
   tabs().retain(|tab| tab.id != id);
}

/// Drops `plugin`'s tabs. True when it had any.
pub fn remove_plugin(plugin: &str) -> bool {
   let mut tabs = tabs();
   let before = tabs.len();
   tabs.retain(|tab| tab.plugin != plugin);
   tabs.len() != before
}
