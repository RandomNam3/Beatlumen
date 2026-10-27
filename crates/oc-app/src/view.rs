//! Navigation and transient UI state.

use crate::host::{Key, Mods};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Library,
    Queue,
    Playlists,
    Backgrounds,
    Effects,
    Settings,
}

impl Page {
    pub const ALL: [Page; 6] = [Page::Library, Page::Queue, Page::Playlists, Page::Backgrounds, Page::Effects, Page::Settings];
    pub fn label(self) -> &'static str {
        match self {
            Page::Library => "Library",
            Page::Queue => "Queue",
            Page::Playlists => "Playlists",
            Page::Backgrounds => "Backgrounds",
            Page::Effects => "Effects",
            Page::Settings => "Settings",
        }
    }
    pub fn icon(self) -> oc_ui::Icon {
        match self {
            Page::Library => oc_ui::Icon::Library,
            Page::Queue => oc_ui::Icon::Queue,
            Page::Playlists => oc_ui::Icon::Playlist,
            Page::Backgrounds => oc_ui::Icon::Image,
            Page::Effects => oc_ui::Icon::Sparkle,
            Page::Settings => oc_ui::Icon::Tune,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibraryTab {
    Tracks,
    Albums,
    Artists,
    Folders,
}

/// Library drill-down targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LibraryDetail {
    Album(String),
    Artist(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Dialog {
    /// Text prompt: title, value, action.
    Prompt { title: String, value: String, action: PromptAction },
    Confirm { title: String, message: String, action: ConfirmAction },
    SavePresetAs(String),
    FileBrowser(FileBrowser),
    AddToPlaylist { items: Vec<String>, kind: oc_net::api::PlaylistKind },
    Login(String),
    About,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PromptAction {
    NewPlaylist { kind: oc_net::api::PlaylistKind, items: Vec<String> },
    RenamePlaylist(String),
    RenameEffect(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConfirmAction {
    DeletePlaylist(String),
    DeletePreset(String),
    ClearQueue,
    ClearBackgrounds,
    RemoveLibraryFolder(String),
}

/// What the file browser was opened for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowseFor {
    LibraryFolder,
    AudioFiles,
    BackgroundFiles,
    BackgroundFolder,
    ImportPreset,
    ExportPreset,
    UploadImages,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FileBrowser {
    pub purpose: BrowseFor,
    pub path: String,
    pub selected: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    pub text: String,
    pub error: bool,
    pub until: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectsTab {
    Effects,
    Analysis,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    General,
    Overlay,
    Server,
    Library,
    About,
}

pub struct View {
    pub menu_open: bool,
    pub page: Page,
    pub library_tab: LibraryTab,
    pub library_detail: Option<LibraryDetail>,
    pub folder_path: Vec<String>,
    pub search: String,
    pub focus_search: bool,
    pub queue_tab: usize,
    pub playlist_open: Option<String>,
    pub effects_tab: EffectsTab,
    pub selected_effect: Option<String>,
    pub settings_tab: SettingsTab,
    pub dialog: Option<Dialog>,
    pub toasts: Vec<Toast>,
    pub theater: bool,
    pub analysis_requested: bool,
    /// Files dropped into the browser (name → bytes).
    pub memory_files: HashMap<String, Vec<u8>>,
    pub pending_playlist_items: Option<(String, Vec<String>)>,
    pub upload_next_drop: bool,
    pub pending_keys: Vec<(Key, Mods)>,
    pub touch_release_clear: bool,
    pub text_input_active: bool,
    pub popup_open: bool,
    pub server_url_edit: String,
    pub server_password: String,
    pub images_tab: usize,
    pub image_folder_input: String,
    pub seek_preview: Option<f64>,
    /// Cached search result: (library revision, query, track indices).
    pub filter_cache: Option<(u64, String, Vec<usize>)>,
    /// Items a context menu was opened for.
    pub context_items: Vec<String>,
}

impl Default for View {
    fn default() -> Self {
        View {
            menu_open: false,
            page: Page::Library,
            library_tab: LibraryTab::Tracks,
            library_detail: None,
            folder_path: Vec::new(),
            search: String::new(),
            focus_search: false,
            queue_tab: 0,
            playlist_open: None,
            effects_tab: EffectsTab::Effects,
            selected_effect: None,
            settings_tab: SettingsTab::General,
            dialog: None,
            toasts: Vec::new(),
            theater: false,
            analysis_requested: false,
            memory_files: HashMap::new(),
            pending_playlist_items: None,
            upload_next_drop: false,
            pending_keys: Vec::new(),
            touch_release_clear: false,
            text_input_active: false,
            popup_open: false,
            server_url_edit: String::new(),
            server_password: String::new(),
            images_tab: 0,
            image_folder_input: String::new(),
            seek_preview: None,
            filter_cache: None,
            context_items: Vec::new(),
        }
    }
}

impl View {
    /// Navigates one level back; returns false when already at the top.
    pub fn go_back(&mut self) -> bool {
        if self.library_detail.is_some() {
            self.library_detail = None;
            return true;
        }
        if self.playlist_open.is_some() {
            self.playlist_open = None;
            return true;
        }
        if self.selected_effect.is_some() {
            self.selected_effect = None;
            return true;
        }
        if !self.folder_path.is_empty() && self.library_tab == LibraryTab::Folders {
            self.folder_path.pop();
            return true;
        }
        false
    }
}
