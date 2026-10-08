use crate::{
    models::{CaptionEditBatch, CaptionEditRule, EditApplyResult, EditPreview, ImageJob},
    storage::{Storage, CAPTION_SOURCE_BATCH_EDIT},
};
use base64::{engine::general_purpose::STANDARD, Engine};
use encoding_rs::GB18030;
use image::ImageFormat;
use regex::{Regex, RegexBuilder};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Cursor,
    path::{Path, PathBuf},
};
use uuid::Uuid;
use walkdir::WalkDir;

const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp", "bmp", "tif", "tiff"];
pub const AI_SELECTED_DIR: &str = ".ai-selected";
pub const AI_REJECTED_DIR: &str = ".ai-rejected";

fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|v| v.to_str())
        .map(|v| IMAGE_EXTENSIONS.contains(&v.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn caption_path(image: &Path) -> PathBuf {
    image.with_extension("txt")
}

pub fn read_text(path: &Path) -> Result<String, String> {
    if !path.exists() {
        return Ok(String::new());
    }
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    if let Ok(value) = String::from_utf8(bytes.clone()) {
        return Ok(value.trim_start_matches('\u{feff}').to_string());
    }
    let (decoded, _, _) = GB18030.decode(&bytes);
    Ok(decoded.into_owned())
}

fn validate_text_file(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Err("文件不存在".into());
    }
    if !path.is_file() {
        return Err("不是普通文件".into());
    }
    let ext = path.extension().and_then(|v| v.to_str()).unwrap_or("");
    if !ext.eq_ignore_ascii_case("txt") {
        return Err("不是 .txt 文本文件".into());
    }
    let metadata = fs::metadata(path).map_err(|e| e.to_string())?;
    if metadata.permissions().readonly() {
        return Err("文件为只读，无法写入".into());
    }
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    if bytes.contains(&0u8) {
        return Err("文件包含二进制内容，不是有效文本".into());
    }
    if String::from_utf8(bytes.clone()).is_err() {
        let (decoded, _, had_errors) = GB18030.decode(&bytes);
        if had_errors && decoded.contains('\u{fffd}') {
            return Err("无法识别的文本编码".into());
        }
    }
    Ok(())
}

/// Windows `fs::canonicalize` 返回 `\\?\` 前缀的扩展路径，用户界面与
/// 文件名比较都不需要它；这里统一转回普通路径形式（`\\?\UNC\` → `\\`）。
pub fn normalize_display_path(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy().into_owned();
    let cleaned = if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        text
    };
    PathBuf::from(cleaned)
}

/// 已复核完成的 caption 路径集合（小写，用于复核去重标记）。
pub type ReviewedSet = std::collections::HashSet<String>;

/// 每张图被明确成功修订的次数（caption 路径小写 → 次数）。
pub type RevisionCounts = std::collections::HashMap<String, u64>;

pub fn image_job(
    path: &Path,
    reviewed: &ReviewedSet,
    revision_counts: &RevisionCounts,
) -> Result<ImageJob, String> {
    let canonical =
        normalize_display_path(fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()));
    let caption = caption_path(&canonical);
    Ok(ImageJob {
        id: Uuid::new_v4().to_string(),
        image_path: canonical.to_string_lossy().to_string(),
        caption_path: caption.to_string_lossy().to_string(),
        file_name: canonical
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("image")
            .to_string(),
        thumbnail: None,
        caption: read_text(&caption)?,
        reviewed: reviewed.contains(&caption.to_string_lossy().to_lowercase()),
        revision_count: revision_counts
            .get(&caption.to_string_lossy().to_lowercase())
            .copied()
            .unwrap_or(0),
        status: if caption.exists() {
            "succeeded".into()
        } else {
            "pending".into()
        },
        selected: false,
        error: None,
        error_kind: None,
        refusal: None,
        metrics: None,
        key_label: None,
        updated_at: None,
        sample_score: None,
    })
}

pub fn select_images(
    multiple: bool,
    reviewed: &ReviewedSet,
    revision_counts: &RevisionCounts,
) -> Result<Vec<ImageJob>, String> {
    let dialog = rfd::FileDialog::new().add_filter("图片", IMAGE_EXTENSIONS);
    let paths = if multiple {
        dialog.pick_files().unwrap_or_default()
    } else {
        dialog.pick_file().into_iter().collect()
    };
    paths
        .into_iter()
        .filter(|path| is_image(path))
        .map(|path| image_job(&path, reviewed, revision_counts))
        .collect()
}

/// 扫描文件夹内全部图片（递归，跳过隐藏项），返回排序去重后的路径。
pub fn scan_images(folder: &Path, recursive: bool) -> Result<Vec<std::path::PathBuf>, String> {
    let mut paths = Vec::new();
    if recursive {
        let entries = WalkDir::new(folder)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| {
                entry.depth() == 0 || !entry.file_name().to_string_lossy().starts_with('.')
            });
        for entry in entries.filter_map(Result::ok) {
            if entry.file_type().is_file() && is_image(entry.path()) {
                paths.push(entry.path().to_path_buf());
            }
        }
    } else {
        for entry in fs::read_dir(folder)
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
        {
            let path = entry.path();
            if path.is_file() && is_image(&path) {
                paths.push(path);
            }
        }
    }
    paths.sort_by_key(|path| path.to_string_lossy().to_ascii_lowercase());
    let mut seen = HashSet::new();
    Ok(paths
        .into_iter()
        .filter(|path| seen.insert(path.to_string_lossy().to_ascii_lowercase()))
        .collect())
}

pub fn select_folder(
    recursive: bool,
    reviewed: &ReviewedSet,
    revision_counts: &RevisionCounts,
) -> Result<Vec<ImageJob>, String> {
    let Some(folder) = rfd::FileDialog::new().pick_folder() else {
        return Ok(Vec::new());
    };
    scan_images(&folder, recursive)?
        .into_iter()
        .map(|path| image_job(&path, reviewed, revision_counts))
        .collect()
}

/// 随机抽选 `count` 张图片；不足时全取。返回 (抽中的, 其余)。
pub fn sample_paths(
    folder: &Path,
    recursive: bool,
    count: usize,
) -> Result<(Vec<std::path::PathBuf>, Vec<std::path::PathBuf>), String> {
    let mut paths = scan_images(folder, recursive)?;
    if paths.is_empty() {
        return Err("文件夹中没有图片".into());
    }
    fastrand::shuffle(&mut paths);
    let take = count.min(paths.len());
    let kept: Vec<_> = paths.drain(..take).collect();
    Ok((kept, paths))
}

/// 把未保留的图片移动到 `<folder>/.refusal/`。同名冲突时跳过并报告。
pub fn apply_sample_refusal(
    folder: &Path,
    kept: &[String],
) -> Result<(Vec<String>, Vec<String>), String> {
    let refusal_dir = folder.join(".refusal");
    let mut moved = Vec::new();
    let mut skipped = Vec::new();
    if kept.is_empty() {
        // 空清单视为全部保留（防误移）
        return Ok((moved, skipped));
    }
    let kept_set: HashSet<String> = kept.iter().map(|p| p.to_ascii_lowercase()).collect();
    let all = scan_images(folder, false)?;
    for path in all {
        let lower = path.to_string_lossy().to_ascii_lowercase();
        if kept_set.contains(&lower) {
            continue;
        }
        let file_name = path.file_name().and_then(|v| v.to_str()).unwrap_or("image");
        if path
            .to_string_lossy()
            .to_ascii_lowercase()
            .starts_with(&refusal_dir.to_string_lossy().to_ascii_lowercase())
        {
            continue; // 已在 .refusal 内
        }
        let target = refusal_dir.join(file_name);
        if target.exists() {
            skipped.push(path.to_string_lossy().into_owned());
            continue;
        }
        std::fs::create_dir_all(&refusal_dir).map_err(|e| e.to_string())?;
        std::fs::rename(&path, &target)
            .map_err(|e| format!("移动 {} 失败：{e}", path.display()))?;
        moved.push(path.to_string_lossy().into_owned());
    }
    Ok((moved, skipped))
}

fn ai_relative_path<'a>(root: &'a Path, source: &'a Path) -> Result<&'a Path, String> {
    source
        .strip_prefix(root.join(AI_SELECTED_DIR))
        .or_else(|_| source.strip_prefix(root.join(AI_REJECTED_DIR)))
        .or_else(|_| source.strip_prefix(root))
        .map_err(|_| "图片路径不在所选抽样文件夹内".to_string())
}

fn move_image_and_caption(source: &Path, target: &Path) -> Result<(), String> {
    if source == target {
        return Ok(());
    }
    if target.exists() {
        return Err(format!("目标文件已存在：{}", target.display()));
    }
    let parent = target.parent().ok_or("目标目录无效")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    fs::rename(source, target).map_err(|e| format!("移动 {} 失败：{e}", source.display()))?;
    let source_caption = caption_path(source);
    if source_caption.exists() {
        let target_caption = caption_path(target);
        if target_caption.exists() {
            let _ = fs::rename(target, source);
            return Err(format!("目标标签文件已存在：{}", target_caption.display()));
        }
        if let Err(error) = fs::rename(&source_caption, &target_caption) {
            let _ = fs::rename(target, source);
            return Err(format!(
                "移动标签 {} 失败：{error}",
                source_caption.display()
            ));
        }
    }
    Ok(())
}

/// AI 每得到一个明确判断就立即移动图片及同名标签；目录保持原相对结构。
pub fn move_ai_sample_image(
    folder: &Path,
    source: &Path,
    keep: bool,
) -> Result<(String, String), String> {
    let root =
        normalize_display_path(fs::canonicalize(folder).map_err(|e| format!("抽样目录无效：{e}"))?);
    let source =
        normalize_display_path(fs::canonicalize(source).map_err(|e| format!("图片不存在：{e}"))?);
    let relative = ai_relative_path(&root, &source)?;
    let bucket = if keep {
        AI_SELECTED_DIR
    } else {
        AI_REJECTED_DIR
    };
    let target = root.join(bucket).join(relative);
    move_image_and_caption(&source, &target)?;
    Ok((
        target.to_string_lossy().into_owned(),
        caption_path(&target).to_string_lossy().into_owned(),
    ))
}

/// 把单个已通过/已丢弃项目退回原相对位置，供再次加入“待分析”队列。
pub fn return_ai_sample_image(folder: &Path, source: &Path) -> Result<(String, String), String> {
    let root =
        normalize_display_path(fs::canonicalize(folder).map_err(|e| format!("抽样目录无效：{e}"))?);
    let source =
        normalize_display_path(fs::canonicalize(source).map_err(|e| format!("图片不存在：{e}"))?);
    let relative = source
        .strip_prefix(root.join(AI_SELECTED_DIR))
        .or_else(|_| source.strip_prefix(root.join(AI_REJECTED_DIR)))
        .map_err(|_| "图片不在 AI 结果目录中".to_string())?;
    let target = root.join(relative);
    move_image_and_caption(&source, &target)?;
    Ok((
        target.to_string_lossy().into_owned(),
        caption_path(&target).to_string_lossy().into_owned(),
    ))
}

/// 把 AI 抽样目录中的图片与同名标签恢复到原相对位置；冲突项保留并报告。
pub fn reset_ai_sample(folder: &Path) -> Result<(Vec<String>, Vec<String>), String> {
    let root =
        normalize_display_path(fs::canonicalize(folder).map_err(|e| format!("抽样目录无效：{e}"))?);
    let mut restored = Vec::new();
    let mut skipped = Vec::new();
    for bucket in [AI_SELECTED_DIR, AI_REJECTED_DIR] {
        let bucket_root = root.join(bucket);
        if !bucket_root.exists() {
            continue;
        }
        let images = scan_images(&bucket_root, true)?;
        for source in images {
            let relative = source
                .strip_prefix(&bucket_root)
                .map_err(|e| e.to_string())?;
            let target = root.join(relative);
            if target.exists() || caption_path(&target).exists() {
                skipped.push(source.to_string_lossy().into_owned());
                continue;
            }
            move_image_and_caption(&source, &target)?;
            restored.push(target.to_string_lossy().into_owned());
        }
        // 仅在已没有图片时清理目录，冲突项仍会原样保留。
        if scan_images(&bucket_root, true)?.is_empty() {
            let _ = fs::remove_dir_all(&bucket_root);
        }
    }
    Ok((restored, skipped))
}

pub fn select_text_files() -> Result<Vec<String>, String> {
    let dialog = rfd::FileDialog::new()
        .add_filter("标签文本", &["txt"])
        .add_filter("全部文件", &["*"]);
    Ok(dialog
        .pick_files()
        .unwrap_or_default()
        .into_iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect())
}

pub fn select_text_folder(recursive: bool) -> Result<Vec<String>, String> {
    let Some(folder) = rfd::FileDialog::new().pick_folder() else {
        return Ok(Vec::new());
    };
    let mut paths = Vec::new();
    let walk = WalkDir::new(&folder).follow_links(false).into_iter();
    let entries = if recursive {
        walk.filter_entry(|entry| {
            entry.depth() == 0 || !entry.file_name().to_string_lossy().starts_with('.')
        })
        .collect::<Vec<_>>()
    } else {
        walk.filter(|entry| {
            entry
                .as_ref()
                .map(|entry| entry.depth() <= 1)
                .unwrap_or(true)
        })
        .collect::<Vec<_>>()
    };
    for entry in entries.into_iter().filter_map(Result::ok) {
        if entry.file_type().is_file() {
            let path = entry.path();
            if path
                .extension()
                .and_then(|v| v.to_str())
                .map(|v| v.eq_ignore_ascii_case("txt"))
                .unwrap_or(false)
            {
                paths.push(path.to_string_lossy().into_owned());
            }
        }
    }
    paths.sort_by_key(|path| path.to_ascii_lowercase());
    Ok(paths)
}

pub fn load_thumbnails(paths: &[String]) -> HashMap<String, String> {
    paths
        .iter()
        .filter_map(|path| {
            let image = image::open(path).ok()?;
            let thumb = image.thumbnail(192, 192);
            let mut bytes = Cursor::new(Vec::new());
            thumb.write_to(&mut bytes, ImageFormat::Jpeg).ok()?;
            Some((
                path.clone(),
                format!(
                    "data:image/jpeg;base64,{}",
                    STANDARD.encode(bytes.into_inner())
                ),
            ))
        })
        .collect()
}

/// Full-size preview: decodes the image and fits it into a `max_dimension`
/// box (square), preserving aspect ratio. Returns a JPEG data URL.
pub fn image_preview(path: &str, max_dimension: u32) -> Result<Option<String>, String> {
    let max_dimension = max_dimension.clamp(256, 4096);
    let image = image::open(path).map_err(|e| format!("无法读取图片：{e}"))?;
    let resized = if image.width() > max_dimension || image.height() > max_dimension {
        image.thumbnail(max_dimension, max_dimension)
    } else {
        image
    };
    let mut bytes = Cursor::new(Vec::new());
    resized
        .write_to(&mut bytes, ImageFormat::Jpeg)
        .map_err(|e| e.to_string())?;
    Ok(Some(format!(
        "data:image/jpeg;base64,{}",
        STANDARD.encode(bytes.into_inner())
    )))
}

fn matcher(pattern: &str, rule: &CaptionEditRule) -> Result<Regex, String> {
    let escaped;
    let expression = if rule.regex {
        pattern
    } else {
        escaped = regex::escape(pattern);
        &escaped
    };
    RegexBuilder::new(expression)
        .case_insensitive(!rule.case_sensitive)
        .multi_line(rule.mode == "text")
        .build()
        .map_err(|e| e.to_string())
}

fn tag_matches(tag: &str, expression: &Regex) -> bool {
    expression.is_match(tag)
}

fn apply_tag(input: &str, rule: &CaptionEditRule) -> Result<String, String> {
    let mut tags: Vec<String> = input
        .split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(String::from)
        .collect();
    let inserted: Vec<String> = rule
        .content
        .split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(String::from)
        .collect();
    let needle = rule
        .anchor
        .as_deref()
        .filter(|v| !v.is_empty())
        .unwrap_or(&rule.content);
    let expression = matcher(needle, rule)?;
    match rule.action.as_str() {
        "delete" => {
            let mut removed = false;
            tags.retain(|tag| {
                if tag_matches(tag, &expression) && (rule.all_matches || !removed) {
                    removed = true;
                    false
                } else {
                    true
                }
            });
        }
        "replace" => {
            let replacements: Vec<String> = rule
                .replacement
                .as_deref()
                .unwrap_or("")
                .split(',')
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(String::from)
                .collect();
            let mut output = Vec::new();
            let mut replaced = false;
            for tag in tags {
                if tag_matches(&tag, &expression) && (rule.all_matches || !replaced) {
                    output.extend(replacements.clone());
                    replaced = true;
                } else {
                    output.push(tag);
                }
            }
            tags = output;
        }
        "insert" => {
            // 仅当缺失时插入：已存在的标签不重复添加（大小写不敏感）。
            let inserted = if rule.only_if_missing {
                let existing: Vec<String> = tags.iter().map(|t| t.to_lowercase()).collect();
                let missing: Vec<String> = inserted
                    .into_iter()
                    .filter(|tag| !existing.contains(&tag.to_lowercase()))
                    .collect();
                if missing.is_empty() {
                    return Ok(input.to_string());
                }
                missing
            } else {
                inserted
            };
            match rule.position.as_str() {
                "start" => {
                    let mut value = inserted;
                    value.extend(tags);
                    tags = value;
                }
                "end" => tags.extend(inserted),
                "index" => {
                    let at = rule.index.unwrap_or(1).saturating_sub(1).min(tags.len());
                    tags.splice(at..at, inserted);
                }
                "before_anchor" | "after_anchor" => {
                    let after = rule.position == "after_anchor";
                    let mut output = Vec::new();
                    let mut matched = false;
                    for tag in tags {
                        let hit = tag_matches(&tag, &expression) && (rule.all_matches || !matched);
                        if hit && !after {
                            output.extend(inserted.clone());
                        }
                        output.push(tag);
                        if hit && after {
                            output.extend(inserted.clone());
                        }
                        matched |= hit;
                    }
                    tags = output;
                }
                _ => return Err("Tag 模式不支持该插入位置".into()),
            }
        }
        _ => return Err("未知编辑操作".into()),
    }
    if rule.deduplicate {
        let mut seen = HashSet::new();
        tags.retain(|tag| {
            seen.insert(if rule.case_sensitive {
                tag.clone()
            } else {
                tag.to_lowercase()
            })
        });
    }
    Ok(tags.join(", "))
}

fn insert_at_char(input: &str, index: usize, content: &str) -> String {
    let byte_index = input
        .char_indices()
        .nth(index)
        .map(|(i, _)| i)
        .unwrap_or(input.len());
    format!(
        "{}{}{}",
        &input[..byte_index],
        content,
        &input[byte_index..]
    )
}

fn apply_text(input: &str, rule: &CaptionEditRule) -> Result<String, String> {
    match rule.action.as_str() {
        "delete" | "replace" => {
            let regex = matcher(&rule.content, rule)?;
            let replacement = if rule.action == "replace" {
                rule.replacement.as_deref().unwrap_or("")
            } else {
                ""
            };
            Ok(if rule.all_matches {
                regex.replace_all(input, replacement).into_owned()
            } else {
                regex.replace(input, replacement).into_owned()
            })
        }
        "insert" => {
            // 仅当缺失时插入：内容已包含目标文本则跳过，避免重复添加。
            if rule.only_if_missing {
                let haystack = if rule.case_sensitive {
                    input.to_string()
                } else {
                    input.to_lowercase()
                };
                let needle = if rule.case_sensitive {
                    rule.content.clone()
                } else {
                    rule.content.to_lowercase()
                };
                if haystack.contains(&needle) {
                    return Ok(input.to_string());
                }
            }
            match rule.position.as_str() {
                "start" => Ok(format!("{}{}", rule.content, input)),
                "end" => Ok(format!("{}{}", input, rule.content)),
                "index" => Ok(insert_at_char(
                    input,
                    rule.index.unwrap_or(0),
                    &rule.content,
                )),
                "line" => {
                    let mut lines: Vec<&str> = input.lines().collect();
                    let at = rule.index.unwrap_or(1).saturating_sub(1).min(lines.len());
                    lines.insert(at, &rule.content);
                    Ok(lines.join("\n"))
                }
                "before_anchor" | "after_anchor" => {
                    let anchor = rule.anchor.as_deref().unwrap_or("");
                    let regex = matcher(anchor, rule)?;
                    let replace = if rule.position == "before_anchor" {
                        format!("{}$0", rule.content)
                    } else {
                        format!("$0{}", rule.content)
                    };
                    Ok(if rule.all_matches {
                        regex.replace_all(input, replace).into_owned()
                    } else {
                        regex.replace(input, replace).into_owned()
                    })
                }
                _ => Err("未知文本插入位置".into()),
            }
        }
        _ => Err("未知编辑操作".into()),
    }
}

pub fn apply_rule(input: &str, rule: &CaptionEditRule) -> Result<String, String> {
    if rule.mode == "tag" {
        apply_tag(input, rule)
    } else {
        apply_text(input, rule)
    }
}

fn dedupe_paths(paths: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    paths
        .iter()
        .filter(|path| seen.insert(path.to_ascii_lowercase()))
        .cloned()
        .collect()
}

/// Builds the per-file preview list, flagging nonexistent, duplicate,
/// read-only or invalid files with explicit errors.
pub fn preview_caption_edit(paths: &[String], rule: &CaptionEditRule) -> Vec<EditPreview> {
    dedupe_paths(paths)
        .iter()
        .map(|path| {
            let path_ref = Path::new(path);
            if let Err(error) = validate_text_file(path_ref) {
                return EditPreview {
                    path: path.clone(),
                    before: String::new(),
                    after: String::new(),
                    changed: false,
                    error: Some(error),
                };
            }
            let before = read_text(path_ref).unwrap_or_default();
            match apply_rule(&before, rule) {
                Ok(after) => EditPreview {
                    path: path.clone(),
                    changed: before != after,
                    before,
                    after,
                    error: None,
                },
                Err(error) => EditPreview {
                    path: path.clone(),
                    before,
                    after: String::new(),
                    changed: false,
                    error: Some(error),
                },
            }
        })
        .collect()
}

/// Creates an edit batch: validates the target files, computes the preview
/// snapshot and persists it so `apply_caption_edit` can verify that no file
/// changed in between.
pub fn preview_caption_edit_batch(
    storage: &Storage,
    paths: &[String],
    rule: &CaptionEditRule,
) -> Result<CaptionEditBatch, String> {
    let previews = preview_caption_edit(paths, rule);
    let affected = previews.iter().filter(|preview| preview.changed).count() as u64;
    let errors = previews
        .iter()
        .filter(|preview| preview.error.is_some())
        .count() as u64;
    let skipped = previews.len() as u64 - affected - errors;
    let id = Uuid::new_v4().to_string();
    storage.create_edit_batch(
        &id,
        &serde_json::to_string(rule).map_err(|e| e.to_string())?,
        &serde_json::to_string(&previews).map_err(|e| e.to_string())?,
        affected,
        skipped,
        errors,
    )?;
    Ok(CaptionEditBatch {
        id,
        rule: rule.clone(),
        previews,
        affected,
        skipped,
        errors,
        created_at: chrono::Utc::now().to_rfc3339(),
        applied: false,
        results: None,
    })
}

/// Applies a previewed batch. Files whose current content differs from the
/// preview snapshot are left untouched and reported as conflicts.
pub fn apply_caption_edit(
    storage: &Storage,
    preview_batch_id: &str,
) -> Result<CaptionEditBatch, String> {
    let (rule_json, previews_json, _, _, _, applied) =
        storage.get_edit_batch_full(preview_batch_id)?;
    if applied {
        return Err("该批次已经应用过".into());
    }
    let rule: CaptionEditRule =
        serde_json::from_str(&rule_json).map_err(|e| format!("批次规则损坏：{e}"))?;
    let previews: Vec<EditPreview> =
        serde_json::from_str(&previews_json).map_err(|e| format!("批次快照损坏：{e}"))?;
    let mut results = Vec::new();
    let mut applied_count = 0u64;
    for preview in &previews {
        if let Some(error) = &preview.error {
            results.push(EditApplyResult {
                path: preview.path.clone(),
                status: "error".into(),
                error: Some(error.clone()),
            });
            continue;
        }
        if !preview.changed {
            results.push(EditApplyResult {
                path: preview.path.clone(),
                status: "skipped".into(),
                error: None,
            });
            continue;
        }
        let current = match read_text(Path::new(&preview.path)) {
            Ok(value) => value,
            Err(error) => {
                results.push(EditApplyResult {
                    path: preview.path.clone(),
                    status: "error".into(),
                    error: Some(error),
                });
                continue;
            }
        };
        if current != preview.before {
            results.push(EditApplyResult {
                path: preview.path.clone(),
                status: "conflict".into(),
                error: Some("文件在预览后被外部修改，已跳过".into()),
            });
            continue;
        }
        match storage.commit_mutation(
            &preview.path,
            &preview.before,
            &preview.after,
            CAPTION_SOURCE_BATCH_EDIT,
            Some(preview_batch_id),
            "batch_apply",
        ) {
            Ok(Some(_)) => {
                applied_count += 1;
                results.push(EditApplyResult {
                    path: preview.path.clone(),
                    status: "applied".into(),
                    error: None,
                });
            }
            Ok(None) => {
                results.push(EditApplyResult {
                    path: preview.path.clone(),
                    status: "skipped".into(),
                    error: None,
                });
            }
            Err(error) => {
                results.push(EditApplyResult {
                    path: preview.path.clone(),
                    status: "error".into(),
                    error: Some(error),
                });
            }
        }
    }
    let results_json = serde_json::to_string(&results).map_err(|e| e.to_string())?;
    storage.apply_edit_batch(preview_batch_id, applied_count, &results_json)?;
    Ok(CaptionEditBatch {
        id: preview_batch_id.into(),
        rule,
        previews,
        affected: applied_count,
        skipped: results.iter().filter(|r| r.status == "skipped").count() as u64,
        errors: results.iter().filter(|r| r.status == "error").count() as u64,
        created_at: chrono::Utc::now().to_rfc3339(),
        applied: true,
        results: Some(results),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Storage;

    fn make_rule(mode: &str, action: &str, position: &str, content: &str) -> CaptionEditRule {
        CaptionEditRule {
            mode: mode.into(),
            action: action.into(),
            position: position.into(),
            content: content.into(),
            replacement: None,
            anchor: None,
            index: Some(1),
            regex: false,
            case_sensitive: false,
            all_matches: true,
            deduplicate: true,
            only_if_missing: false,
        }
    }

    #[test]
    fn text_insert_skips_when_content_already_present() {
        let mut rule = make_rule("text", "insert", "end", "quality_masterpiece");
        rule.only_if_missing = true;
        // 已包含：跳过
        assert_eq!(
            apply_rule("1girl, quality_masterpiece", &rule).unwrap(),
            "1girl, quality_masterpiece"
        );
        // 不包含：追加到结尾
        assert_eq!(
            apply_rule("1girl", &rule).unwrap(),
            "1girlquality_masterpiece"
        );
        // 开头插入同理
        rule.position = "start".into();
        assert_eq!(
            apply_rule("1girl", &rule).unwrap(),
            "quality_masterpiece1girl"
        );
        // 大小写不敏感：已包含 "Quality_Masterpiece" 时跳过
        rule.position = "end".into();
        assert_eq!(
            apply_rule("1girl, Quality_Masterpiece", &rule).unwrap(),
            "1girl, Quality_Masterpiece"
        );
        // case_sensitive=true 时大小写不同视为缺失
        rule.case_sensitive = true;
        assert_ne!(
            apply_rule("1girl, Quality_Masterpiece", &rule).unwrap(),
            "1girl, Quality_Masterpiece"
        );
        // only_if_missing=false 行为不变（总是插入）
        let mut plain = make_rule("text", "insert", "end", "smile");
        plain.only_if_missing = false;
        assert_eq!(
            apply_rule("1girl, smile", &plain).unwrap(),
            "1girl, smilesmile"
        );
    }

    #[test]
    fn tag_insert_only_adds_missing_tags() {
        let mut rule = make_rule("tag", "insert", "end", "smile, solo");
        rule.only_if_missing = true;
        // smile 已存在：只补 solo
        assert_eq!(
            apply_rule("1girl, smile", &rule).unwrap(),
            "1girl, smile, solo"
        );
        // 全部已存在：无变化
        assert_eq!(
            apply_rule("1girl, smile, solo", &rule).unwrap(),
            "1girl, smile, solo"
        );
        // 全缺失：正常添加
        assert_eq!(apply_rule("1girl", &rule).unwrap(), "1girl, smile, solo");
    }

    #[test]
    fn inserts_and_deduplicates_tags() {
        assert_eq!(
            apply_rule(
                "1girl, solo, 1girl",
                &make_rule("tag", "insert", "end", "smile")
            )
            .unwrap(),
            "1girl, solo, smile"
        );
    }
    #[test]
    fn inserts_by_anchor() {
        let mut value = make_rule("tag", "insert", "before_anchor", "smile");
        value.anchor = Some("outdoors".into());
        assert_eq!(
            apply_rule("1girl, outdoors", &value).unwrap(),
            "1girl, smile, outdoors"
        );
    }
    #[test]
    fn replaces_text_regex() {
        let mut value = make_rule("text", "replace", "end", r"\d+");
        value.regex = true;
        value.replacement = Some("N".into());
        assert_eq!(apply_rule("foo 12 bar", &value).unwrap(), "foo N bar");
    }
    #[test]
    fn tag_semantics_first_vs_all_and_case() {
        // First match only.
        let mut first = make_rule("tag", "delete", "end", "bad");
        first.all_matches = false;
        assert_eq!(apply_rule("bad, good, bad", &first).unwrap(), "good, bad");
        // Case-insensitive delete.
        assert_eq!(
            apply_rule("BAd, good", &make_rule("tag", "delete", "end", "bad")).unwrap(),
            "good"
        );
        // Case-sensitive delete keeps the differently-cased tag.
        let mut sensitive = make_rule("tag", "delete", "end", "bad");
        sensitive.case_sensitive = true;
        assert_eq!(apply_rule("BAd, good", &sensitive).unwrap(), "BAd, good");
        // Replace first occurrence with multiple tags (dedup disabled so the
        // untouched duplicate stays).
        let mut replace = make_rule("tag", "replace", "end", "solo");
        replace.all_matches = false;
        replace.deduplicate = false;
        replace.replacement = Some("solo, focus".into());
        assert_eq!(
            apply_rule("solo, solo, 1girl", &replace).unwrap(),
            "solo, focus, solo, 1girl"
        );
        // With dedup enabled the duplicate collapses.
        let mut dedup = replace.clone();
        dedup.deduplicate = true;
        assert_eq!(
            apply_rule("solo, solo, 1girl", &dedup).unwrap(),
            "solo, focus, 1girl"
        );
    }
    #[test]
    fn text_semantics_multiline_and_anchors() {
        let mut line = make_rule("text", "insert", "line", "note");
        line.index = Some(2);
        assert_eq!(apply_rule("a\nb", &line).unwrap(), "a\nnote\nb");
        let mut anchor = make_rule("text", "insert", "after_anchor", "!");
        anchor.anchor = Some("end".to_string());
        anchor.all_matches = true;
        assert_eq!(
            apply_rule("the end is near", &anchor).unwrap(),
            "the end! is near"
        );
        let mut replace_all = make_rule("text", "replace", "end", "bad");
        replace_all.regex = true;
        replace_all.replacement = Some("good".to_string());
        assert_eq!(apply_rule("bad bad", &replace_all).unwrap(), "good good");
        // Multiline text delete with ^ anchor.
        let mut multiline = make_rule("text", "delete", "end", "^todo ");
        multiline.regex = true;
        multiline.all_matches = true;
        assert_eq!(apply_rule("todo a\ntodo b", &multiline).unwrap(), "a\nb");
    }
    #[test]
    fn preview_batch_flow_detects_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image.txt");
        fs::write(&path, "1girl, solo").unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        let batch = preview_caption_edit_batch(
            &storage,
            &[path.to_string_lossy().to_string()],
            &make_rule("tag", "insert", "end", "smile"),
        )
        .unwrap();
        assert_eq!(batch.affected, 1);
        assert!(!batch.applied);
        // External modification between preview and apply.
        fs::write(&path, "1boy, solo").unwrap();
        let result = apply_caption_edit(&storage, &batch.id).unwrap();
        assert_eq!(result.results.as_ref().unwrap()[0].status, "conflict");
        assert_eq!(fs::read_to_string(&path).unwrap(), "1boy, solo");
        // Applying a second time is rejected.
        assert!(apply_caption_edit(&storage, &batch.id).is_err());
    }
    #[test]
    fn preview_batch_flow_applies_and_records_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image.txt");
        fs::write(&path, "1girl, solo").unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        let batch = preview_caption_edit_batch(
            &storage,
            &[path.to_string_lossy().to_string()],
            &make_rule("tag", "insert", "end", "smile"),
        )
        .unwrap();
        let result = apply_caption_edit(&storage, &batch.id).unwrap();
        assert_eq!(result.results.as_ref().unwrap()[0].status, "applied");
        assert_eq!(fs::read_to_string(&path).unwrap(), "1girl, solo, smile");
        assert_eq!(
            storage
                .list_versions(Some(path.to_string_lossy().as_ref()))
                .unwrap()
                .len(),
            1
        );
        assert!(storage.get_edit_batch_full(&batch.id).unwrap().5);
    }
    #[test]
    fn preview_flags_invalid_files() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.txt");
        let read_only = dir.path().join("locked.txt");
        fs::write(&read_only, "1girl").unwrap();
        let mut permissions = fs::metadata(&read_only).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&read_only, permissions).unwrap();
        let previews = preview_caption_edit(
            &[
                missing.to_string_lossy().to_string(),
                read_only.to_string_lossy().to_string(),
            ],
            &make_rule("tag", "insert", "end", "smile"),
        );
        assert_eq!(previews.len(), 2);
        assert!(previews[0].error.as_deref().unwrap().contains("不存在"));
        assert!(previews[1].error.as_deref().unwrap().contains("只读"));
    }
    #[test]
    fn strips_windows_extended_path_prefix() {
        assert_eq!(
            normalize_display_path(PathBuf::from(r"\\?\D:\exium - 副本\140146542_p4.txt")),
            PathBuf::from(r"D:\exium - 副本\140146542_p4.txt")
        );
        assert_eq!(
            normalize_display_path(PathBuf::from(r"\\?\UNC\server\share\a.txt")),
            PathBuf::from(r"\\server\share\a.txt")
        );
        // 普通路径原样保留。
        assert_eq!(
            normalize_display_path(PathBuf::from(r"D:\data\img.png")),
            PathBuf::from(r"D:\data\img.png")
        );
    }

    #[test]
    fn preview_fits_images_into_bounding_box() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.png");
        let image = image::RgbaImage::from_pixel(4000, 2000, image::Rgba([255u8, 0, 0, 255]));
        image.save(&path).unwrap();
        let preview = image_preview(path.to_str().unwrap(), 800).unwrap().unwrap();
        assert!(preview.starts_with("data:image/jpeg;base64,"));
        let bytes = STANDARD
            .decode(&preview.as_bytes()["data:image/jpeg;base64,".len()..])
            .unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!(decoded.width(), 800);
        assert_eq!(decoded.height(), 400);
    }

    #[test]
    fn ai_sample_moves_immediately_and_reset_restores_relative_paths_and_caption() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("nested");
        fs::create_dir_all(&nested).unwrap();
        let image = nested.join("person.png");
        image::RgbaImage::from_pixel(16, 16, image::Rgba([1, 2, 3, 255]))
            .save(&image)
            .unwrap();
        fs::write(nested.join("person.txt"), "existing tags").unwrap();

        let (selected, selected_caption) = move_ai_sample_image(dir.path(), &image, true).unwrap();
        assert!(selected.contains(AI_SELECTED_DIR));
        assert!(Path::new(&selected).exists());
        assert_eq!(
            fs::read_to_string(&selected_caption).unwrap(),
            "existing tags"
        );

        let (rejected, _) = move_ai_sample_image(dir.path(), Path::new(&selected), false).unwrap();
        assert!(rejected.contains(AI_REJECTED_DIR));
        assert!(!Path::new(&selected).exists());

        let (pending, _) = return_ai_sample_image(dir.path(), Path::new(&rejected)).unwrap();
        assert_eq!(Path::new(&pending), image);
        assert!(image.exists());
        let (selected_again, _) = move_ai_sample_image(dir.path(), &image, true).unwrap();
        assert!(selected_again.contains(AI_SELECTED_DIR));

        let (restored, skipped) = reset_ai_sample(dir.path()).unwrap();
        assert!(skipped.is_empty());
        assert_eq!(restored.len(), 1);
        assert!(image.exists());
        assert_eq!(
            fs::read_to_string(nested.join("person.txt")).unwrap(),
            "existing tags"
        );
    }
}
