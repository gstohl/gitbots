//! A thin wasm-bindgen shim over the Cloudflare Artifacts Workers binding
//! (workers-rs has none). Types follow `@cloudflare/workers-types`
//! `ArtifactsRepo` / `Artifacts` (>= 5.20260930.1).

use gitbots_cloud::source::{
    Commit, EntryKind, SourceError, TreeEntry, TreeSource, walk_first_parents,
};
use js_sys::{Promise, Reflect, Uint8Array};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use worker::{Env, EnvBinding};

#[wasm_bindgen]
extern "C" {
    /// The namespace binding (`[[artifacts]]` in wrangler.toml).
    #[wasm_bindgen(extends = js_sys::Object)]
    #[derive(Clone, Debug)]
    pub type ArtifactsSys;

    #[wasm_bindgen(method, catch)]
    fn create(this: &ArtifactsSys, name: &str, opts: &JsValue) -> Result<Promise, JsValue>;
    #[wasm_bindgen(method, catch)]
    fn get(this: &ArtifactsSys, name: &str) -> Result<Promise, JsValue>;
    #[wasm_bindgen(method, catch, js_name = delete)]
    fn delete_repo(this: &ArtifactsSys, name: &str) -> Result<Promise, JsValue>;

    /// A repo handle (an RPC stub; disposed on drop).
    #[wasm_bindgen(extends = js_sys::Object)]
    #[derive(Clone, Debug)]
    pub type RepoSys;

    #[wasm_bindgen(method, catch)]
    fn info(this: &RepoSys) -> Result<Promise, JsValue>;
    #[wasm_bindgen(method, catch, js_name = createToken)]
    fn create_token(this: &RepoSys, scope: &str, ttl: f64) -> Result<Promise, JsValue>;
    #[wasm_bindgen(method, catch, js_name = listTokens)]
    fn list_tokens(this: &RepoSys) -> Result<Promise, JsValue>;
    #[wasm_bindgen(method, catch, js_name = revokeToken)]
    fn revoke_token(this: &RepoSys, token_or_id: &str) -> Result<Promise, JsValue>;
    #[wasm_bindgen(method, catch)]
    fn fork(this: &RepoSys, name: &str, opts: &JsValue) -> Result<Promise, JsValue>;
    #[wasm_bindgen(method, catch)]
    fn log(this: &RepoSys, opts: &JsValue) -> Result<Promise, JsValue>;
    #[wasm_bindgen(method, catch, js_name = readCommit)]
    fn read_commit(this: &RepoSys, hash: &str) -> Result<Promise, JsValue>;
    #[wasm_bindgen(method, catch, js_name = readTree)]
    fn read_tree(this: &RepoSys, hash: &str) -> Result<Promise, JsValue>;
    #[wasm_bindgen(method, catch, js_name = readBlob)]
    fn read_blob(this: &RepoSys, hash: &str) -> Result<Promise, JsValue>;
    #[wasm_bindgen(method, catch, js_name = readFile)]
    fn read_file(this: &RepoSys, args: &JsValue) -> Result<Promise, JsValue>;

    #[wasm_bindgen(extends = js_sys::Object)]
    #[derive(Clone, Debug)]
    type BlobSys;

    #[wasm_bindgen(method, js_name = arrayBuffer)]
    fn array_buffer(this: &BlobSys) -> Promise;
}

impl EnvBinding for ArtifactsSys {
    const TYPE_NAME: &'static str = "Artifacts";

    // The binding is an RPC stub whose constructor name is not a stable
    // contract, so skip the type check.
    fn get(val: JsValue) -> worker::Result<Self> {
        if val.is_object() {
            Ok(val.unchecked_into())
        } else {
            Err(worker::Error::RustError("ARTIFACTS binding is not an object".into()))
        }
    }
}

/// An `ArtifactsError` (or any other exception) from the binding.
#[derive(Clone, Debug)]
pub struct ArtifactsError {
    /// `NOT_FOUND`, `ALREADY_EXISTS`, `INVALID_REPO_NAME`, ... when the
    /// binding threw an `ArtifactsError`.
    pub code: Option<String>,
    pub message: String,
}

impl std::fmt::Display for ArtifactsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.code {
            Some(code) => write!(f, "artifacts {code}: {}", self.message),
            None => write!(f, "artifacts: {}", self.message),
        }
    }
}

impl ArtifactsError {
    fn from_js(e: JsValue) -> Self {
        let get =
            |k: &str| Reflect::get(&e, &JsValue::from_str(k)).ok().and_then(|v| v.as_string());
        let message = get("message").or_else(|| e.as_string()).unwrap_or_else(|| format!("{e:?}"));
        Self { code: get("code"), message }
    }

    fn decode(what: &str, e: impl std::fmt::Display) -> Self {
        Self { code: None, message: format!("unexpected {what} shape: {e}") }
    }

    pub fn is(&self, code: &str) -> bool {
        self.code.as_deref() == Some(code)
    }
}

type Res<T> = Result<T, ArtifactsError>;

async fn call(p: Result<Promise, JsValue>) -> Res<JsValue> {
    let p = p.map_err(ArtifactsError::from_js)?;
    JsFuture::from(p).await.map_err(ArtifactsError::from_js)
}

fn decode<T: DeserializeOwned>(what: &str, v: JsValue) -> Res<T> {
    serde_wasm_bindgen::from_value(v).map_err(|e| ArtifactsError::decode(what, e))
}

fn to_js<T: Serialize>(v: &T) -> JsValue {
    // Plain objects (not Maps) for the binding's options.
    let ser = serde_wasm_bindgen::Serializer::json_compatible();
    v.serialize(&ser).unwrap_or(JsValue::UNDEFINED)
}

/// `ArtifactsCreateRepoResult` (create, fork, import).
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedRepo {
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub default_branch: Option<String>,
    pub remote: String,
    /// Plaintext token (`art_v2_x_...` live; the docs say `art_v1_...?expires=`).
    pub token: String,
}

/// `ArtifactsCreateTokenResult`.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssuedToken {
    pub id: String,
    pub plaintext: String,
    #[serde(default)]
    pub scope: Option<String>,
    /// ISO 8601.
    pub expires_at: String,
}

/// `ArtifactsCommitMetadata`.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitMeta {
    pub hash: String,
    pub tree_hash: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub parents: Vec<String>,
}

/// `ArtifactsTreeEntry`.
#[derive(Clone, Debug, Deserialize)]
pub struct TreeEntrySys {
    pub name: String,
    pub mode: String,
    pub hash: String,
    /// `tree` | `blob` | `symlink` | `gitlink` | `exec`.
    #[serde(rename = "type")]
    pub kind: String,
}

impl TreeEntrySys {
    fn into_entry(self) -> TreeEntry {
        let mode = TreeEntry::parse_mode(&self.mode).unwrap_or(match self.kind.as_str() {
            "tree" => 0o40000,
            "gitlink" => 0o160000,
            "symlink" => 0o120000,
            "exec" => 0o100755,
            _ => 0o100644,
        });
        let kind = match self.kind.as_str() {
            "tree" => EntryKind::Tree,
            "gitlink" => EntryKind::Commit,
            "blob" | "symlink" | "exec" => EntryKind::Blob,
            _ => TreeEntry::kind_of_mode(mode),
        };
        TreeEntry { name: self.name, sha: self.hash, kind, mode }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateOpts<'a> {
    description: &'a str,
    set_default_branch: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ForkOpts<'a> {
    description: &'a str,
    read_only: bool,
    default_branch_only: bool,
}

#[derive(Serialize)]
struct LogOpts<'a> {
    #[serde(rename = "ref")]
    reference: &'a str,
    limit: u32,
}

#[derive(Serialize)]
struct FileArgs<'a> {
    #[serde(rename = "ref")]
    reference: &'a str,
    path: &'a str,
}

pub struct Artifacts(ArtifactsSys);

impl Artifacts {
    pub fn from_env(env: &Env) -> worker::Result<Self> {
        env.get_binding::<ArtifactsSys>("ARTIFACTS").map(Self)
    }

    pub async fn create(
        &self,
        name: &str,
        description: &str,
        default_branch: &str,
    ) -> Res<CreatedRepo> {
        let opts = to_js(&CreateOpts { description, set_default_branch: default_branch });
        decode("create result", call(self.0.create(name, &opts)).await?)
    }

    pub async fn get(&self, name: &str) -> Res<Repo> {
        let v = call(self.0.get(name)).await?;
        if v.is_null() || v.is_undefined() {
            return Err(ArtifactsError { code: Some("NOT_FOUND".into()), message: name.into() });
        }
        Ok(Repo { sys: v.unchecked_into(), name: name.to_owned() })
    }

    pub async fn delete(&self, name: &str) -> Res<bool> {
        Ok(call(self.0.delete_repo(name)).await?.as_bool().unwrap_or(false))
    }
}

/// A handle to one repo. Disposes the RPC stub when dropped.
pub struct Repo {
    sys: RepoSys,
    pub name: String,
}

impl Drop for Repo {
    fn drop(&mut self) {
        let dispose = Reflect::get(&js_sys::global(), &"Symbol".into())
            .and_then(|symbol| Reflect::get(&symbol, &"dispose".into()))
            .and_then(|key| Reflect::get(&self.sys, &key));
        if let Ok(f) = dispose.and_then(|f| f.dyn_into::<js_sys::Function>()) {
            let _ = f.call0(&self.sys);
        }
    }
}

impl Repo {
    /// Raw `info()` result, for diagnostics.
    pub async fn info_raw(&self) -> Res<JsValue> {
        call(self.sys.info()).await
    }

    pub async fn create_token(&self, write: bool, ttl_secs: u64) -> Res<IssuedToken> {
        let scope = if write { "write" } else { "read" };
        // ttl is clamped by the caller to the binding's 60..=31536000.
        #[allow(clippy::cast_precision_loss)]
        let ttl = ttl_secs as f64;
        decode("createToken result", call(self.sys.create_token(scope, ttl)).await?)
    }

    /// Raw `listTokens()` result, for diagnostics.
    pub async fn tokens_raw(&self) -> Res<JsValue> {
        call(self.sys.list_tokens()).await
    }

    /// Revokes a token by plaintext or id; `false` if there was none.
    pub async fn revoke_token(&self, token_or_id: &str) -> Res<bool> {
        Ok(call(self.sys.revoke_token(token_or_id)).await?.as_bool().unwrap_or(false))
    }

    pub async fn fork(&self, name: &str, description: &str) -> Res<CreatedRepo> {
        // All branches: the fork must carry `gitbots/activity` and the base.
        let opts = to_js(&ForkOpts { description, read_only: false, default_branch_only: false });
        decode("fork result", call(self.sys.fork(name, &opts)).await?)
    }

    /// First-parent log from `reference` (branch, tag or commit), newest first.
    /// Empty if the ref does not resolve.
    pub async fn log(&self, reference: &str, limit: u32) -> Res<Vec<CommitMeta>> {
        let opts = to_js(&LogOpts { reference, limit: limit.clamp(1, 1000) });
        decode("log result", call(self.sys.log(&opts)).await?)
    }

    pub async fn log_raw(&self, reference: &str, limit: u32) -> Res<JsValue> {
        call(self.sys.log(&to_js(&LogOpts { reference, limit }))).await
    }

    /// The commit a branch (or other ref) points at.
    pub async fn tip(&self, reference: &str) -> Res<Option<String>> {
        Ok(self.log(reference, 1).await?.into_iter().next().map(|c| c.hash))
    }

    pub async fn commit(&self, hash: &str) -> Res<Option<CommitMeta>> {
        let v = call(self.sys.read_commit(hash)).await?;
        if v.is_null() || v.is_undefined() { Ok(None) } else { decode("commit", v).map(Some) }
    }

    pub async fn tree_raw(&self, hash: &str) -> Res<JsValue> {
        call(self.sys.read_tree(hash)).await
    }

    pub async fn tree(&self, hash: &str) -> Res<Option<Vec<TreeEntry>>> {
        let v = self.tree_raw(hash).await?;
        if v.is_null() || v.is_undefined() {
            return Ok(None);
        }
        let entries: Vec<TreeEntrySys> = decode("tree", v)?;
        Ok(Some(entries.into_iter().map(TreeEntrySys::into_entry).collect()))
    }

    pub async fn blob(&self, hash: &str) -> Res<Option<Vec<u8>>> {
        blob_bytes(call(self.sys.read_blob(hash)).await?).await
    }

    /// A file at `path` on `reference`.
    pub async fn file(&self, reference: &str, path: &str) -> Res<Option<Vec<u8>>> {
        let args = to_js(&FileArgs { reference, path });
        blob_bytes(call(self.sys.read_file(&args)).await?).await
    }
}

async fn blob_bytes(v: JsValue) -> Res<Option<Vec<u8>>> {
    if v.is_null() || v.is_undefined() {
        return Ok(None);
    }
    let blob: BlobSys = v.unchecked_into();
    let buf = JsFuture::from(blob.array_buffer()).await.map_err(ArtifactsError::from_js)?;
    Ok(Some(Uint8Array::new(&buf).to_vec()))
}

fn source_err(e: ArtifactsError) -> SourceError {
    if e.is("NOT_FOUND") {
        SourceError::NotFound { what: "object", id: e.message }
    } else {
        SourceError::Backend(e.to_string())
    }
}

impl TreeSource for Repo {
    async fn read_commit(&self, sha: &str) -> Result<Commit, SourceError> {
        let c = self
            .commit(sha)
            .await
            .map_err(source_err)?
            .ok_or_else(|| SourceError::NotFound { what: "commit", id: sha.to_owned() })?;
        Ok(Commit { sha: c.hash, tree: c.tree_hash, parents: c.parents, message: c.message })
    }

    async fn read_tree(&self, sha: &str) -> Result<Vec<TreeEntry>, SourceError> {
        self.tree(sha)
            .await
            .map_err(source_err)?
            .ok_or_else(|| SourceError::NotFound { what: "tree", id: sha.to_owned() })
    }

    async fn read_blob(&self, sha: &str) -> Result<Vec<u8>, SourceError> {
        self.blob(sha)
            .await
            .map_err(source_err)?
            .ok_or_else(|| SourceError::NotFound { what: "blob", id: sha.to_owned() })
    }

    async fn first_parent_history(
        &self,
        sha: &str,
        limit: usize,
    ) -> Result<Vec<String>, SourceError> {
        // One `log()` call instead of a `readCommit` per commit.
        let n = u32::try_from(limit.min(1000)).unwrap_or(1000);
        let log = self.log(sha, n).await.map_err(source_err)?;
        if log.is_empty() {
            return walk_first_parents(self, sha, limit).await;
        }
        Ok(log.into_iter().map(|c| c.hash).collect())
    }
}
