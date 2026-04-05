//! XDG desktop file and icon handling.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Simple loader for app icons.
#[derive(Debug)]
pub struct IconLoader {
    icons: HashMap<String, (String, ImageType, PathBuf)>,
    app_ids: HashMap<String, Option<PathBuf>>,

    user_dir: PathBuf,
    sys_dir: PathBuf,
}

impl IconLoader {
    /// Initialize the icon loader.
    ///
    /// This will check all paths for available icons and store them for cheap
    /// lookup.
    pub fn new() -> Self {
        let mut icons: HashMap<String, (String, ImageType, PathBuf)> = HashMap::new();
        let user_dir = dirs::data_dir().unwrap_or_default();
        let sys_dir = PathBuf::from("/usr/share/");

        // NOTE: Themes are checked in order of priority, if an icon is found in a theme
        // of lesser priority, it is ignored completely regardless of how low
        // quality the existing icon might be.

        // Iterate on all XDG_DATA_DIRS to look for icons.
        for data_dir in [&user_dir, &sys_dir] {
            // Iterate over theme fallback list in descending importance.
            for theme in themes_for_dir(&data_dir.join("icons")) {
                let theme_dir = data_dir.join("icons").join(&theme);
                for dir_entry in fs::read_dir(&theme_dir).into_iter().flatten().flatten() {
                    // Get last path segment from directory.
                    let dir_name = match dir_entry.file_name().into_string() {
                        Ok(dir_name) => dir_name,
                        Err(_) => continue,
                    };

                    // Handle standardized icon theme directory layout.
                    let image_type = if dir_name == "scalable" {
                        ImageType::Scalable
                    } else if dir_name == "symbolic" {
                        ImageType::Symbolic
                    } else if let Some((width, height)) = dir_name.split_once('x') {
                        match (u32::from_str(width), u32::from_str(height)) {
                            (Ok(width), Ok(height)) if width == height => {
                                ImageType::SizedBitmap(width)
                            },
                            _ => continue,
                        }
                    } else {
                        continue;
                    };

                    // Iterate over all files in all category subdirectories.
                    let categories = fs::read_dir(dir_entry.path()).into_iter().flatten().flatten();
                    for file in categories.flat_map(|c| fs::read_dir(c.path())).flatten().flatten()
                    {
                        // Get last path segment from file.
                        let file_name = match file.file_name().into_string() {
                            Ok(file_name) => file_name,
                            Err(_) => continue,
                        };

                        // Strip extension.
                        let name = match (file_name.rsplit_once('.'), image_type) {
                            (Some(("", _)), _) => continue,
                            (Some((name, _)), ImageType::Symbolic) => {
                                match name.strip_suffix("-symbolic") {
                                    Some(name) => name,
                                    None => continue,
                                }
                            },
                            (Some((name, _)), _) => name,
                            (None, _) => continue,
                        };

                        // Store the icon path, unless a better one was found already.
                        match icons.entry(name.to_owned()) {
                            Entry::Occupied(entry) => {
                                let (existing_theme, existing_type, path) = entry.into_mut();

                                // Ignore lower-priority themes.
                                if existing_theme != &theme {
                                    continue;
                                }

                                // Replace icon if a bigger/scaleable one is found.
                                if image_type > *existing_type {
                                    *path = file.path();
                                }
                            },
                            Entry::Vacant(entry) => {
                                entry.insert((theme.clone(), image_type, file.path()));
                            },
                        }
                    }
                }
            }
        }

        // Add pixmaps first, this path is hardcoded in the specification.
        for file in fs::read_dir("/usr/share/pixmaps").into_iter().flatten().flatten() {
            // Get last path segment from file.
            let file_name = match file.file_name().into_string() {
                Ok(file_name) => file_name,
                Err(_) => continue,
            };

            // Determine image type based on extension.
            let (name, image_type) = match file_name.rsplit_once('.') {
                Some((name, "svg")) => (name, ImageType::Scalable),
                // We don’t have any information about the size of the icon here.
                Some((name, "png")) => (name, ImageType::Bitmap),
                _ => continue,
            };

            // Add icon to our icon loader.
            if !icons.contains_key(name) {
                icons.insert(name.to_owned(), (String::new(), image_type, file.path()));
            }
        }

        Self { user_dir, sys_dir, icons, app_ids: Default::default() }
    }

    /// Get an icon from its Wayland `app_id`.
    pub fn icon_path(&mut self, app_id: &str) -> Option<&Path> {
        // Populate app_id cache from icon cache.
        if !self.app_ids.contains_key(app_id) {
            // Get all possible desktop file paths.
            let desktop_file = format!("{app_id}.desktop");
            let desktop_file_lowercase = format!("{}.desktop", app_id.to_ascii_lowercase());
            let desktop_dirs = [
                self.user_dir.join("applications").join(&desktop_file),
                self.user_dir.join("applications").join(&desktop_file_lowercase),
                self.sys_dir.join("applications").join(&desktop_file),
                self.sys_dir.join("applications").join(&desktop_file_lowercase),
            ];

            // Try and extract the icon name from any desktop file.
            let icon_name = desktop_dirs
                .into_iter()
                .filter_map(|path| fs::read_to_string(path).ok())
                .flat_map(|content| {
                    content.lines().find_map(|line| line.strip_prefix("Icon=")).map(String::from)
                })
                .next();

            // Load the path from the icon cache.
            let icon_path = icon_name.and_then(|name| Some(self.icons.get(&name)?.2.clone()));

            // Update the icon cache.
            self.app_ids.insert(app_id.to_owned(), icon_path);
        }

        self.app_ids.get(app_id).unwrap().as_deref()
    }
}

/// Expected type of an image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ImageType {
    /// A bitmap image of a known square size.
    SizedBitmap(u32),

    /// A bitmap image of an unknown size.
    Bitmap,

    /// A vector image.
    Scalable,

    /// A monochrome vector image.
    Symbolic,
}

impl Ord for ImageType {
    fn cmp(&self, other: &Self) -> Ordering {
        if self == other {
            return Ordering::Equal;
        }

        match (self, other) {
            // Prefer scaleable formats.
            (Self::Scalable, _) => Ordering::Greater,
            (_, Self::Scalable) => Ordering::Less,
            // Prefer bigger bitmap sizes.
            (Self::SizedBitmap(size), Self::SizedBitmap(other_size)) => size.cmp(other_size),
            // Prefer bitmaps with known size.
            (Self::SizedBitmap(_), _) => Ordering::Greater,
            (_, Self::SizedBitmap(_)) => Ordering::Less,
            // Prefer bitmaps over symbolic icons without color.
            (Self::Bitmap, _) => Ordering::Greater,
            (_, Self::Bitmap) => Ordering::Less,
            // Equality is checked by the gate clause already.
            (Self::Symbolic, Self::Symbolic) => unreachable!(),
        }
    }
}

impl PartialOrd for ImageType {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Recursively parse theme specs to find theme fallback hierarchy.
fn themes_for_dir(root_dir: &Path) -> Vec<String> {
    let mut all_themes = vec!["default".into()];
    let mut index = 0;

    while index < all_themes.len() {
        // Add theme's dependencies to theme list.
        let index_path = root_dir.join(&all_themes[index]).join("index.theme");
        let mut themes = parse_index(&index_path);
        all_themes.append(&mut themes);

        // Deduplicate themes list, to avoid redundant work.
        for i in (0..all_themes.len()).rev() {
            if all_themes[..i].contains(&all_themes[i]) {
                all_themes.remove(i);
            }
        }

        index += 1;
    }

    all_themes
}

/// Parse index.theme and extract `Inherits` attribute.
fn parse_index(path: &Path) -> Vec<String> {
    // Read entire file.
    let index = match fs::read_to_string(path) {
        Ok(index) => index,
        Err(_) => return Vec::new(),
    };

    // Find `Inherits` attribute start.
    let start = match index.find("Inherits=") {
        Some(start) => start + "Inherits=".len(),
        None => return Vec::new(),
    };

    // Extract `Inherits` value.
    let inherits = match index[start..].find(char::is_whitespace) {
        Some(end) => &index[start..start + end],
        None => &index[start..],
    };

    inherits.split(',').map(|s| s.to_string()).collect()
}
