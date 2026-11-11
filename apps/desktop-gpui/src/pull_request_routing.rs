//! Pull request routing: machines this device trusts read, and may act on, pull requests
//! through each other's source control access. Ports `pullRequestRoutingPermissions.ts` (the
//! per-machine grant, keyed to the saved endpoint) and `createPullRequestRouter` in
//! `packages/client-runtime/src/state/pullRequestRouting.ts`. Credentials stay on their
//! environments; only a host and an account the origin verified cross the wire.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::time::Duration;

use futures::future::{BoxFuture, FutureExt as _, Shared};
use futures::{StreamExt as _, stream};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use url::Url;

use crate::environments::{Address, Route};
use crate::rpc::{RpcClient, RpcError};
use crate::ssh::SshTarget;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Permission {
    #[default]
    Off,
    Read,
    ReadWrite,
}

/// The picker's options, in order.
pub const PERMISSIONS: [(Permission, &str); 3] =
    [(Permission::Off, "Off"), (Permission::Read, "Read PRs"), (Permission::ReadWrite, "Read and act")];

impl Permission {
    pub fn label(self) -> &'static str {
        PERMISSIONS.iter().find(|(permission, _)| *permission == self).map_or("Off", |(_, label)| label)
    }
}

/// `StoredPullRequestRoutingPermission`: a grant, valid only while the endpoint it was given to
/// keeps the same `connection_key`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stored {
    pub environment_id: String,
    pub connection_key: String,
    pub permission: Permission,
}

/// `pullRequestRoutingConnectionKey`: trust belongs to the saved endpoint, never to an environment
/// id a server advertises, so a changed address revokes it. `None` for an address it cannot key.
pub fn connection_key(primary: bool, environment_id: &str, http_base: &str) -> Option<String> {
    let http = Url::parse(http_base).ok()?;
    if !matches!(http.scheme(), "http" | "https") || !http.username().is_empty() || http.password().is_some() {
        return None;
    }
    let mut ws = http.clone();
    ws.set_scheme(if http.scheme() == "https" { "wss" } else { "ws" }).ok()?;
    let tag = if primary { "PrimaryConnectionTarget" } else { "BearerConnectionTarget" };
    let href = |url: &Url| url.as_str().trim_end_matches('/').to_owned();
    Some(json!([tag, environment_id, href(&http), href(&ws)]).to_string())
}

/// [`connection_key`] for a saved environment over all its routes: one route keys alone, several
/// as their sorted keys, so reordering keeps trust but adding or changing an address revokes it.
pub fn saved_connection_key(environment_id: &str, routes: &[Route]) -> Option<String> {
    let mut keys = routes.iter().map(|route| route_key(environment_id, route)).collect::<Option<Vec<_>>>()?;
    if keys.len() == 1 {
        return keys.pop();
    }
    // `Array.prototype.sort` orders by UTF-16 code units.
    keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    Some(json!(keys).to_string())
}

fn route_key(environment_id: &str, route: &Route) -> Option<String> {
    match &route.address {
        Address::Direct(http_base) => connection_key(false, environment_id, http_base),
        Address::Ssh(SshTarget { alias, hostname, username, port }) => {
            Some(json!(["SshConnectionTarget", environment_id, alias, hostname, username, port]).to_string())
        }
    }
}

/// `pullRequestRoutingPermissionFor`.
pub fn permission_for(key: Option<&str>, stored: &[Stored]) -> Permission {
    key.and_then(|key| stored.iter().find(|grant| grant.connection_key == key))
        .map_or(Permission::Off, |g| g.permission)
}

/// `PullRequestRoutingPermissions.set`: replaces the environment's grant; Off removes it.
pub fn set(stored: &mut Vec<Stored>, environment_id: &str, connection_key: String, permission: Permission) {
    stored.retain(|grant| grant.environment_id != environment_id);
    if permission != Permission::Off {
        stored.push(Stored { environment_id: environment_id.to_owned(), connection_key, permission });
    }
}

/// `summarizePullRequestRouting`: `A, B read and act · C read PRs`, or `None` when all are off.
pub fn summarize(entries: &[(&str, Permission)]) -> Option<String> {
    let groups: Vec<String> = [(Permission::ReadWrite, "read and act"), (Permission::Read, "read PRs")]
        .into_iter()
        .filter_map(|(permission, label)| {
            let labels: Vec<&str> = entries.iter().filter(|(_, p)| *p == permission).map(|(l, _)| *l).collect();
            (!labels.is_empty()).then(|| format!("{} {label}", labels.join(", ")))
        })
        .collect();
    (!groups.is_empty()).then(|| groups.join(" \u{b7} "))
}

/// `isLocal`: the machine is reached over loopback.
pub fn is_local(http_base: &str) -> bool {
    Url::parse(http_base)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .is_some_and(|host| matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]"))
}

// ---------------------------------------------------------------------------------------------
// Router

const SUMMARY: &str = "pullRequests.summary";
const DETAIL: &str = "pullRequests.detail";
const CHECKS: &str = "pullRequests.checks";
const INVALIDATE: &str = "pullRequests.invalidate";
const SET_FILES_VIEWED: &str = "pullRequests.setFilesViewed";
const ROUTING: &str = "pullRequests.routing";
const ROUTING_IDENTITY: &str = "pullRequests.routingIdentity";
const REPORT_STATE: &str = "pullRequests.reportState";
const READS: [&str; 12] = [
    SUMMARY,
    "pullRequests.stack",
    DETAIL,
    "pullRequests.preview",
    CHECKS,
    "pullRequests.activity",
    "pullRequests.threadComments",
    "pullRequests.diffFileContents",
    "pullRequests.filesViewed",
    "pullRequests.changeBreakdown",
    "pullRequests.reviewerCandidates",
    "pullRequests.labelCandidates",
];
// Review guides are not routed: each server keeps its own, so another server answering would
// report none, or start a run nobody on the pull request's own server can see.
const WRITES: [&str; 11] = [
    SET_FILES_VIEWED,
    "pullRequests.runAction",
    "pullRequests.update",
    "pullRequests.comment",
    "pullRequests.updateComment",
    "pullRequests.submitReview",
    "pullRequests.replyToThread",
    "pullRequests.setReaction",
    "pullRequests.setThreadResolution",
    "pullRequests.requestReviewers",
    "pullRequests.setLabels",
];
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const READ_TIMEOUT: Duration = Duration::from_secs(30);
const INVALIDATE_TIMEOUT: Duration = Duration::from_secs(1);
const REPORT_TIMEOUT: Duration = Duration::from_secs(5);
/// The routed reads remembered for invalidation, oldest dropped first.
const ROUTED_READS: usize = 256;

/// One environment's socket, as the router calls it.
pub trait Transport: Clone + Send + Sync + 'static {
    /// The request, never routed again.
    fn call(&self, tag: &str, payload: Value) -> BoxFuture<'static, Result<Value, RpcError>>;
    fn same(&self, other: &Self) -> bool;
}

impl Transport for RpcClient {
    fn call(&self, tag: &str, payload: Value) -> BoxFuture<'static, Result<Value, RpcError>> {
        let (client, tag) = (self.clone(), tag.to_owned());
        async move { client.request_direct(&tag, payload).await }.boxed()
    }

    fn same(&self, other: &Self) -> bool {
        self.same_socket(other)
    }
}

/// A connected, enabled, supported environment the router may use.
#[derive(Clone)]
pub struct Member<T> {
    pub environment_id: String,
    pub transport: T,
    pub local: bool,
    pub permission: Permission,
    /// The server answers `pullRequests.checks`.
    pub checks: bool,
}

/// `PullRequestRef`, without the routing fields the router fills in.
#[derive(Clone, Debug, PartialEq)]
struct Ref {
    project_id: String,
    host: Option<String>,
    repository: String,
    number: u64,
}

fn trimmed(value: &Value) -> Option<&str> {
    value.as_str().filter(|text| !text.is_empty() && text.trim() == *text)
}

impl Ref {
    /// `Schema.is(PullRequestRef)`.
    fn parse(input: &Value) -> Option<Self> {
        let optional = |key: &str| match input.get(key) {
            None => Some(None),
            Some(value) => trimmed(value).map(|text| Some(text.to_owned())),
        };
        optional("expectedAccountId")?;
        if input.get("allowStale").is_some_and(|value| !value.is_boolean()) {
            return None;
        }
        Some(Self {
            project_id: input.get("projectId")?.as_str()?.to_owned(),
            host: optional("host")?,
            repository: trimmed(input.get("repository")?)?.to_owned(),
            number: input.get("number")?.as_u64().filter(|number| *number > 0)?,
        })
    }

    fn with_host(&self, host: &str) -> Self {
        Self { host: Some(host.to_owned()), ..self.clone() }
    }

    fn to_value(&self) -> Value {
        let mut value = json!({ "projectId": self.project_id, "repository": self.repository, "number": self.number });
        if let Some(host) = &self.host {
            value["host"] = json!(host);
        }
        value
    }

    /// `matchesReference`.
    fn matches(&self, filter: &Self) -> bool {
        self.project_id == filter.project_id
            && self.repository.eq_ignore_ascii_case(&filter.repository)
            && self.number == filter.number
            && filter
                .host
                .as_ref()
                .is_none_or(|host| self.host.as_ref().is_some_and(|own| own.eq_ignore_ascii_case(host)))
    }

    fn key(&self, origin: &str) -> String {
        let host = self.host.as_ref().map(|host| host.to_lowercase());
        json!([origin, self.project_id, host, self.repository.to_lowercase(), self.number.to_string()]).to_string()
    }
}

/// `PullRequestRoutingResult`: the account the origin verified for the pull request's host.
struct Identity {
    account_id: String,
    host: String,
    provider: String,
    project_title: String,
    workspace_root: String,
}

impl Identity {
    fn parse(value: &Value) -> Option<Self> {
        let field = |key: &str| trimmed(value.get(key)?).map(str::to_owned);
        field("viewer")?;
        Some(Self {
            account_id: field("accountId")?,
            host: field("host")?,
            provider: field("provider")?,
            project_title: field("projectTitle")?,
            workspace_root: field("workspaceRoot")?,
        })
    }

    /// Another machine's `PullRequestRoutingIdentityResult` names the same account.
    fn same_account(&self, other: &Value) -> bool {
        let field = |key: &str| other.get(key).and_then(Value::as_str);
        field("provider") == Some(&self.provider)
            && field("host").is_some_and(|host| host.eq_ignore_ascii_case(&self.host))
            && field("accountId") == Some(&self.account_id)
    }
}

/// A read some other machine answered, so a later write or refresh invalidates it there too.
struct RoutedRead {
    origin: String,
    reference: Ref,
    targets: Vec<String>,
}

type Source = Shared<BoxFuture<'static, Result<Value, RpcError>>>;

fn unavailable(message: &str) -> RpcError {
    RpcError::Failure(json!({ "_tag": "EnvironmentRpcUnavailableError", "message": message }))
}

/// The request, sent only once something awaits it: a shared source the route never needs
/// stays unsent.
fn deferred<T: Transport>(transport: &T, tag: &str, payload: Value) -> BoxFuture<'static, Result<Value, RpcError>> {
    let (transport, tag) = (transport.clone(), tag.to_owned());
    async move { transport.call(&tag, payload).await }.boxed()
}

fn with_read_timeout(
    request: BoxFuture<'static, Result<Value, RpcError>>,
) -> BoxFuture<'static, Result<Value, RpcError>> {
    async move {
        tokio::time::timeout(READ_TIMEOUT, request)
            .await
            .unwrap_or_else(|_| Err(unavailable("The environment did not respond to the PR request.")))
    }
    .boxed()
}

/// `rejectedBeforeDispatch`: the request never reached a server, so another may take it.
fn rejected_before_dispatch(error: &RpcError) -> bool {
    let RpcError::Failure(failure) = error else { return false };
    match failure.get("_tag").and_then(Value::as_str) {
        Some("EnvironmentRpcUnavailableError" | "EnvironmentNotRegisteredError") => true,
        Some("PullRequestOperationError") => failure.get("operation").and_then(Value::as_str) == Some("routeIdentity"),
        _ => false,
    }
}

/// `hasState`: an answer that says whether the pull request is open, closed or merged.
fn reported_state(answer: &Value) -> Option<(&str, Option<bool>)> {
    let state = answer.get("state")?.as_str().filter(|state| matches!(*state, "open" | "closed" | "merged"))?;
    match answer.get("isDraft") {
        None => Some((state, None)),
        Some(Value::Bool(draft)) => Some((state, Some(*draft))),
        Some(_) => None,
    }
}

async fn probe<T: Transport>(transport: &T, tag: &str, payload: Value) -> Option<Value> {
    tokio::time::timeout(PROBE_TIMEOUT, transport.call(tag, payload)).await.ok()?.ok()
}

pub struct Router<T> {
    members: RwLock<Arc<[Member<T>]>>,
    active: AtomicBool,
    routed: Mutex<Vec<(String, RoutedRead)>>,
}

impl<T> Default for Router<T> {
    fn default() -> Self {
        Self { members: RwLock::new(Arc::from(Vec::new())), active: AtomicBool::new(false), routed: Mutex::default() }
    }
}

impl<T: Transport> Router<T> {
    /// Replaces the environments the router knows, with their grants.
    pub fn publish(&self, members: Vec<Member<T>>) {
        let granted = members.iter().filter(|member| member.permission != Permission::Off).count();
        *self.members.write().unwrap_or_else(std::sync::PoisonError::into_inner) = members.into();
        self.active.store(granted >= 2, Ordering::Release);
    }

    /// At least two machines are trusted, so a request may route.
    pub fn active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    fn members(&self) -> Arc<[Member<T>]> {
        self.members.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    fn member(&self, environment_id: &str) -> Option<Member<T>> {
        self.members().iter().find(|member| member.environment_id == environment_id).cloned()
    }

    fn routed_reads(&self) -> std::sync::MutexGuard<'_, Vec<(String, RoutedRead)>> {
        self.routed.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// `routingAllowed`, read again at each step since grants can change meanwhile.
    fn allowed(&self, origin: &str, destination: &str, write: bool) -> bool {
        let members = self.members();
        let permission = |id: &str| members.iter().find(|member| member.environment_id == id).map(|m| m.permission);
        let (Some(source), Some(target)) = (permission(origin), permission(destination)) else { return false };
        if write {
            source == Permission::ReadWrite && target == Permission::ReadWrite
        } else {
            source != Permission::Off && target != Permission::Off
        }
    }

    /// `readOrWrite`: a request `origin` makes, answered by whichever trusted machine can.
    pub async fn request(&self, origin: &T, tag: &str, input: Value) -> Result<Value, RpcError> {
        let members = self.members();
        let Some(source) = members.iter().find(|member| member.transport.same(origin)).cloned() else {
            return origin.call(tag, input).await;
        };
        if !READS.contains(&tag) || Ref::parse(&input).is_none() {
            return self.routed_request(&source, tag, input, None).await;
        }
        let id = source.environment_id.as_str();
        let trusted = members.iter().any(|m| m.environment_id != id && self.allowed(id, &m.environment_id, false));
        if members.len() < 2 || !trusted {
            return origin.call(tag, input).await;
        }
        let mut strict = input.clone();
        strict["allowStale"] = json!(false);
        let read = with_read_timeout(deferred(origin, tag, strict.clone())).shared();
        let result = if source.local {
            match read.clone().await {
                Ok(answer) => Ok(answer),
                Err(_) => self.routed_request(&source, tag, strict, Some(read)).await,
            }
        } else {
            self.routed_request(&source, tag, strict, Some(read)).await
        };
        match result {
            Err(_)
                if input.get("allowStale").and_then(Value::as_bool) != Some(false)
                    && matches!(tag, SUMMARY | DETAIL) =>
            {
                origin.call(tag, input).await
            }
            result => result,
        }
    }

    /// `routedRequest`.
    async fn routed_request(
        &self,
        origin: &Member<T>,
        tag: &str,
        input: Value,
        source_read: Option<Source>,
    ) -> Result<Value, RpcError> {
        if tag == INVALIDATE {
            return self.invalidate(origin, input).await;
        }
        let write = WRITES.contains(&tag);
        let reference = (write || READS.contains(&tag)).then(|| Ref::parse(&input)).flatten();
        let Some(reference) = reference else { return origin.transport.call(tag, input).await };
        let origin_id = origin.environment_id.as_str();
        let alternatives: Vec<(String, bool)> = self
            .members()
            .iter()
            .filter(|m| m.environment_id != origin_id && self.allowed(origin_id, &m.environment_id, write))
            .filter(|m| tag != CHECKS || m.checks)
            .map(|m| (m.environment_id.clone(), m.local))
            .collect();
        let source = || source_read.clone().unwrap_or_else(|| deferred(&origin.transport, tag, input.clone()).shared());
        if alternatives.is_empty() {
            let result = source().await;
            return self.finish(origin_id, tag, &reference, result).await;
        }
        // The source server answers only for a host whose account it can verify.
        let identity = probe(&origin.transport, ROUTING, input.clone()).await.as_ref().and_then(Identity::parse);
        let Some(identity) = identity else {
            let result = source().await;
            return self.finish(origin_id, tag, &reference, result).await;
        };
        let mut routed_input = input.clone();
        routed_input["host"] = json!(identity.host);
        routed_input["expectedAccountId"] = json!(identity.account_id);
        let guarded =
            source_read.clone().unwrap_or_else(|| deferred(&origin.transport, tag, routed_input.clone()).shared());
        let candidates = candidates(origin_id, origin.local, &alternatives, write);
        let read = !write;
        let mut index = 0;
        let result = loop {
            let Some(id) = candidates.get(index) else { break guarded.clone().await };
            index += 1;
            let more = index < candidates.len();
            if id != origin_id && !self.verified(origin_id, id, write, &identity).await {
                continue;
            }
            match self.run(origin, id, tag, &reference, &identity, &routed_input, &guarded, read).await {
                Err(error) if (read || rejected_before_dispatch(&error)) && more => {}
                Err(error) => break Err(error),
                Ok(mut answer) => {
                    if answer.get("projectId").is_some() {
                        answer["projectId"] = json!(reference.project_id);
                        if tag == DETAIL {
                            answer["projectTitle"] = json!(identity.project_title);
                            answer["workspaceRoot"] = json!(identity.workspace_root);
                        }
                    }
                    break Ok(answer);
                }
            }
        };
        self.finish(origin_id, tag, &reference, result).await
    }

    /// The machine is still trusted and its server holds the same account for the host. An older
    /// server would discard `expectedAccountId`, so it must implement the guard first.
    async fn verified(&self, origin: &str, id: &str, write: bool, identity: &Identity) -> bool {
        if !self.allowed(origin, id, write) {
            return false;
        }
        let Some(member) = self.member(id) else { return false };
        let answer = probe(&member.transport, ROUTING_IDENTITY, json!({ "host": identity.host })).await;
        answer.is_some_and(|answer| identity.same_account(&answer)) && self.allowed(origin, id, write)
    }

    async fn run(
        &self,
        origin: &Member<T>,
        id: &str,
        tag: &str,
        reference: &Ref,
        identity: &Identity,
        routed_input: &Value,
        guarded: &Source,
        read: bool,
    ) -> Result<Value, RpcError> {
        let origin_id = origin.environment_id.as_str();
        let request = if id == origin_id {
            guarded.clone().boxed()
        } else {
            let Some(member) = self.member(id) else { return Err(unavailable("The environment was removed.")) };
            member.transport.call(tag, routed_input.clone())
        };
        let answer = if read { with_read_timeout(request).await } else { request.await }?;
        if id != origin_id
            && let Some((state, draft)) = reported_state(&answer)
        {
            // The origin owns the threads linked to this pull request, but this read never reached it.
            let mut report = json!({ "reference": reference.to_value(), "state": state });
            if let Some(draft) = draft {
                report["isDraft"] = json!(draft);
            }
            let request = origin.transport.call(REPORT_STATE, report);
            tokio::spawn(async move { drop(tokio::time::timeout(REPORT_TIMEOUT, request).await) });
        }
        let key = reference.key(origin_id);
        let mut routed = self.routed_reads();
        match routed.iter_mut().find(|(existing, _)| *existing == key) {
            Some((_, entry)) if !entry.targets.iter().any(|target| target == id) => entry.targets.push(id.to_owned()),
            Some(_) => {}
            None => {
                if routed.len() >= ROUTED_READS {
                    routed.remove(0);
                }
                let entry = RoutedRead {
                    origin: origin_id.to_owned(),
                    reference: reference.with_host(&identity.host),
                    targets: vec![id.to_owned()],
                };
                routed.push((key, entry));
            }
        }
        Ok(answer)
    }

    /// A write tells every machine that answered reads of the pull request to read it again.
    /// A failed viewed-files batch may have applied some paths; other writes invalidate on success.
    async fn finish(
        &self,
        origin: &str,
        tag: &str,
        reference: &Ref,
        result: Result<Value, RpcError>,
    ) -> Result<Value, RpcError> {
        let files_viewed = tag == SET_FILES_VIEWED;
        if !WRITES.contains(&tag) || (result.is_err() && !files_viewed) {
            return result;
        }
        let mut targets: Vec<(String, Vec<Ref>)> = vec![(origin.to_owned(), vec![reference.clone()])];
        {
            let routed = self.routed_reads();
            if routed.is_empty() {
                return result;
            }
            for (_, entry) in routed.iter() {
                if entry.origin != origin || !entry.reference.matches(reference) {
                    continue;
                }
                for target in entry.targets.iter().map(String::as_str).chain([origin]) {
                    let index = targets.iter().position(|(id, _)| id == target).unwrap_or_else(|| {
                        targets.push((target.to_owned(), Vec::new()));
                        targets.len() - 1
                    });
                    let refs = &mut targets[index].1;
                    if !refs.iter().any(|existing| existing.host == entry.reference.host) {
                        refs.push(entry.reference.clone());
                    }
                }
            }
        }
        let invalidations = targets.into_iter().map(|(target, refs)| {
            let mut inputs: Vec<Value> = refs
                .iter()
                .map(|reference| {
                    let mut input = json!({ "reference": reference.to_value() });
                    if files_viewed {
                        input["filesViewedOnly"] = json!(true);
                    }
                    input
                })
                .collect();
            if !files_viewed {
                inputs.push(json!({}));
            }
            async move { self.invalidate_target(origin, &target, inputs).await }
        });
        stream::iter(invalidations).for_each_concurrent(4, |invalidation| invalidation).await;
        result
    }

    /// `pullRequests.invalidate` on the origin, then on each machine that answered a matching read.
    async fn invalidate(&self, origin: &Member<T>, input: Value) -> Result<Value, RpcError> {
        let filter = match input.get("reference") {
            None => None,
            Some(reference) => match Ref::parse(reference) {
                Some(reference) => Some(reference),
                None => return origin.transport.call(INVALIDATE, input).await,
            },
        };
        let result = origin.transport.call(INVALIDATE, input.clone()).await?;
        let origin_id = origin.environment_id.as_str();
        let mut targets: Vec<(String, Ref)> = Vec::new();
        let mut put = |target: &str, reference: &Ref| match targets.iter_mut().find(|(id, _)| id == target) {
            Some(existing) => existing.1 = reference.clone(),
            None => targets.push((target.to_owned(), reference.clone())),
        };
        for (_, entry) in self.routed_reads().iter() {
            if entry.origin != origin_id || filter.as_ref().is_some_and(|filter| !entry.reference.matches(filter)) {
                continue;
            }
            for target in &entry.targets {
                put(target, &entry.reference);
            }
            if filter.is_some() {
                put(origin_id, &entry.reference);
            }
        }
        let invalidations = targets.into_iter().map(|(target, reference)| {
            let mut payload = input.clone();
            if filter.is_some() {
                payload["reference"] = reference.to_value();
            }
            async move { self.invalidate_target(origin_id, &target, vec![payload]).await }
        });
        stream::iter(invalidations).for_each_concurrent(4, |invalidation| invalidation).await;
        Ok(result)
    }

    /// `invalidateTarget`. A stalled reader must not delay the write's answer.
    async fn invalidate_target(&self, origin: &str, target: &str, inputs: Vec<Value>) {
        if target != origin && !self.allowed(origin, target, false) {
            return;
        }
        let Some(member) = self.member(target) else { return };
        stream::iter(inputs)
            .for_each_concurrent(3, |input| {
                let request = member.transport.call(INVALIDATE, input);
                async move { drop(tokio::time::timeout(INVALIDATE_TIMEOUT, request).await) }
            })
            .await;
    }
}

/// Who answers, in order: local machines first, then remote ones. A write tries the origin
/// among the local machines when it is local, else before the remote ones; a read asks it last.
fn candidates(origin: &str, origin_local: bool, alternatives: &[(String, bool)], write: bool) -> Vec<String> {
    let of = |local: bool| alternatives.iter().filter(move |(_, l)| *l == local).map(|(id, _)| id.clone());
    let mut candidates = Vec::new();
    if write && origin_local {
        candidates.push(origin.to_owned());
    }
    candidates.extend(of(true));
    if write && !origin_local {
        candidates.push(origin.to_owned());
    }
    candidates.extend(of(false));
    if !write {
        candidates.push(origin.to_owned());
    }
    candidates
}

static ROUTER: LazyLock<Router<RpcClient>> = LazyLock::new(Router::default);

/// The environments the router may use, with this device's grants.
pub fn publish(members: Vec<Member<RpcClient>>) {
    ROUTER.publish(members);
}

/// Whether [`RpcClient::request`] hands `tag` to the router.
pub fn intercepts(tag: &str) -> bool {
    tag.starts_with("pullRequests.")
        && (tag == INVALIDATE || READS.contains(&tag) || WRITES.contains(&tag))
        && ROUTER.active()
}

/// Routes `origin`'s request on the shared runtime, whose timers the router uses.
pub async fn request(origin: &RpcClient, tag: &str, payload: Value) -> Result<Value, RpcError> {
    let (origin, tag) = (origin.clone(), tag.to_owned());
    crate::runtime()
        .spawn(async move { ROUTER.request(&origin, &tag, payload).await })
        .await
        .unwrap_or_else(|error| Err(RpcError::Defect(error.to_string())))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::environments::Credential;

    #[test]
    fn grants_follow_the_saved_endpoint() {
        let key = connection_key(false, "env", "https://box.example:8443/").unwrap();
        assert_eq!(key, r#"["BearerConnectionTarget","env","https://box.example:8443","wss://box.example:8443"]"#);
        assert_eq!(
            connection_key(true, "local", "http://127.0.0.1:3773").unwrap(),
            r#"["PrimaryConnectionTarget","local","http://127.0.0.1:3773","ws://127.0.0.1:3773"]"#
        );
        assert_eq!(connection_key(false, "env", "https://user:pw@box"), None);
        assert_eq!(connection_key(false, "env", "ftp://box"), None);

        let mut stored = Vec::new();
        set(&mut stored, "env", key.clone(), Permission::ReadWrite);
        assert_eq!(permission_for(Some(&key), &stored), Permission::ReadWrite);
        // A new address is a new endpoint: the grant no longer applies.
        let moved = connection_key(false, "env", "https://other.example").unwrap();
        assert_eq!(permission_for(Some(&moved), &stored), Permission::Off);
        assert_eq!(permission_for(None, &stored), Permission::Off);
        set(&mut stored, "env", key.clone(), Permission::Read);
        assert_eq!(stored.len(), 1, "one grant per environment");
        set(&mut stored, "env", key, Permission::Off);
        assert!(stored.is_empty(), "off forgets the grant");
    }

    #[test]
    fn saved_environments_are_trusted_over_every_saved_route() {
        let (credential, target) = (Credential::Missing, SshTarget::alias("box"));
        let ssh = Route { address: Address::Ssh(SshTarget { username: Some("me".into()), ..target }), credential };
        let lan = Route::direct("http://192.168.1.10:3773".into(), Credential::Missing);
        let tailnet = Route::direct("https://box.ts.net".into(), Credential::Missing);
        let ssh_key = r#"["SshConnectionTarget","env","box","box","me",null]"#;
        assert_eq!(saved_connection_key("env", std::slice::from_ref(&ssh)).as_deref(), Some(ssh_key));
        assert_eq!(
            saved_connection_key("env", std::slice::from_ref(&lan)),
            connection_key(false, "env", "http://192.168.1.10:3773"),
            "one route keys as itself"
        );
        let both = saved_connection_key("env", &[lan.clone(), ssh.clone()]).unwrap();
        let lan_key = connection_key(false, "env", "http://192.168.1.10:3773").unwrap();
        assert_eq!(both, json!([lan_key, ssh_key]).to_string(), "sorted keys of every route");
        assert_eq!(saved_connection_key("env", &[ssh.clone(), lan.clone()]).unwrap(), both, "reordering keeps trust");
        let grown = saved_connection_key("env", &[tailnet, lan, ssh.clone()]).unwrap();
        assert_ne!(grown, both, "adding a route revokes trust");
        let unkeyable = Route::direct("ftp://box".into(), Credential::Missing);
        assert_eq!(saved_connection_key("env", &[unkeyable, ssh]), None, "every route must key");
    }

    #[test]
    fn summaries_group_machines_by_grant() {
        use Permission::{Off, Read, ReadWrite};
        assert_eq!(summarize(&[("A", Off), ("B", Off)]), None);
        assert_eq!(
            summarize(&[("A", Read), ("B", ReadWrite), ("C", ReadWrite), ("D", Off)]).as_deref(),
            Some("B, C read and act \u{b7} A read PRs")
        );
        assert_eq!(serde_json::to_value(ReadWrite).unwrap(), "read-write");
    }

    #[test]
    fn local_machines_answer_first_and_reads_ask_the_origin_last() {
        let alternatives = [("remote".to_owned(), false), ("local".to_owned(), true)];
        assert_eq!(candidates("o", true, &alternatives, false), ["local", "remote", "o"]);
        assert_eq!(candidates("o", true, &alternatives, true), ["o", "local", "remote"]);
        assert_eq!(candidates("o", false, &alternatives, true), ["local", "o", "remote"]);
    }

    /// A machine whose server answers each method from a table, recording every call.
    #[derive(Clone)]
    struct Fake {
        name: &'static str,
        answers: Arc<HashMap<&'static str, Result<Value, RpcError>>>,
        calls: Calls,
    }

    impl Transport for Fake {
        fn call(&self, tag: &str, payload: Value) -> BoxFuture<'static, Result<Value, RpcError>> {
            self.calls.lock().unwrap().push((self.name, tag.to_owned(), payload));
            let answer = self.answers.get(tag).cloned().unwrap_or(Ok(Value::Null));
            async move { answer }.boxed()
        }

        fn same(&self, other: &Self) -> bool {
            self.name == other.name
        }
    }

    type Calls = Arc<Mutex<Vec<(&'static str, String, Value)>>>;

    fn fake(name: &'static str, calls: &Calls, answers: &[(&'static str, Result<Value, RpcError>)]) -> Fake {
        Fake { name, answers: Arc::new(answers.iter().cloned().collect()), calls: calls.clone() }
    }

    fn member(fake: &Fake, local: bool, permission: Permission) -> Member<Fake> {
        Member { environment_id: fake.name.to_owned(), transport: fake.clone(), local, permission, checks: true }
    }

    fn identity() -> Value {
        json!({ "accountId": "42", "host": "github.com", "provider": "github", "viewer": "me",
                "projectTitle": "Here", "workspaceRoot": "/here" })
    }

    fn reference() -> Value {
        json!({ "projectId": "p-origin", "repository": "acme/app", "number": 7 })
    }

    fn calls_to(calls: &Calls, name: &str) -> Vec<String> {
        calls.lock().unwrap().iter().filter(|(n, ..)| *n == name).map(|(_, tag, _)| tag.clone()).collect()
    }

    #[tokio::test]
    async fn a_remote_origin_reads_through_a_trusted_local_machine_with_the_verified_account() {
        let calls = Calls::default();
        let origin = fake(
            "origin",
            &calls,
            &[(ROUTING, Ok(identity())), (DETAIL, Ok(json!({ "projectId": "p-origin", "state": "open" })))],
        );
        let local = fake(
            "local",
            &calls,
            &[
                (
                    ROUTING_IDENTITY,
                    Ok(json!({ "accountId": "42", "host": "GitHub.com", "provider": "github", "viewer": "me" })),
                ),
                (DETAIL, Ok(json!({ "projectId": "p-local", "projectTitle": "There", "state": "merged" }))),
            ],
        );
        let router = Router::default();
        router.publish(vec![member(&origin, false, Permission::Read), member(&local, true, Permission::Read)]);
        assert!(router.active());

        let answer = router.request(&origin, DETAIL, reference()).await.unwrap();
        assert_eq!(answer["projectId"], "p-origin", "the answer names the origin's project");
        assert_eq!(answer["projectTitle"], "Here");
        assert_eq!(answer["state"], "merged");
        let sent = calls.lock().unwrap().iter().find(|(n, tag, _)| *n == "local" && tag == DETAIL).cloned().unwrap();
        assert_eq!(sent.2["expectedAccountId"], "42");
        assert_eq!(sent.2["host"], "github.com");
        assert_eq!(sent.2["allowStale"], false);
        tokio::task::yield_now().await;
        assert_eq!(calls_to(&calls, "origin"), [ROUTING, REPORT_STATE], "the origin hears the state it missed");
    }

    #[tokio::test]
    async fn a_machine_holding_another_account_is_skipped() {
        let calls = Calls::default();
        let origin = fake("origin", &calls, &[(ROUTING, Ok(identity())), (DETAIL, Ok(json!({ "state": "open" })))]);
        let other = fake(
            "other",
            &calls,
            &[(
                ROUTING_IDENTITY,
                Ok(json!({ "accountId": "7", "host": "github.com", "provider": "github", "viewer": "x" })),
            )],
        );
        let router = Router::default();
        router.publish(vec![member(&origin, false, Permission::Read), member(&other, true, Permission::Read)]);
        router.request(&origin, DETAIL, reference()).await.unwrap();
        assert_eq!(calls_to(&calls, "other"), [ROUTING_IDENTITY]);
        assert_eq!(calls_to(&calls, "origin"), [ROUTING, DETAIL]);
    }

    #[tokio::test]
    async fn writes_need_read_and_act_on_both_machines_and_invalidate_routed_readers() {
        let calls = Calls::default();
        let origin = fake("origin", &calls, &[(ROUTING, Ok(identity())), ("pullRequests.comment", Ok(json!({})))]);
        let peer = fake(
            "peer",
            &calls,
            &[
                (
                    ROUTING_IDENTITY,
                    Ok(json!({ "accountId": "42", "host": "github.com", "provider": "github", "viewer": "me" })),
                ),
                (DETAIL, Ok(json!({ "projectId": "p", "state": "open" }))),
            ],
        );
        let router = Router::default();
        // Origin read-only: writes stay on the origin.
        router.publish(vec![member(&origin, false, Permission::Read), member(&peer, true, Permission::ReadWrite)]);
        router.request(&origin, DETAIL, reference()).await.unwrap();
        calls.lock().unwrap().clear();
        let mut comment = reference();
        comment["body"] = json!("hi");
        router.request(&origin, "pullRequests.comment", comment).await.unwrap();
        assert_eq!(calls_to(&calls, "origin"), ["pullRequests.comment", INVALIDATE, INVALIDATE, INVALIDATE]);
        // The peer answered a read, so it reads the pull request again.
        assert_eq!(calls_to(&calls, "peer"), [INVALIDATE, INVALIDATE]);
    }

    #[tokio::test]
    async fn without_two_trusted_machines_nothing_routes() {
        let calls = Calls::default();
        let origin = fake("origin", &calls, &[]);
        let peer = fake("peer", &calls, &[]);
        let router = Router::default();
        router.publish(vec![member(&origin, false, Permission::Read), member(&peer, true, Permission::Off)]);
        assert!(!router.active());
        router.request(&origin, DETAIL, reference()).await.unwrap();
        assert_eq!(calls_to(&calls, "origin"), [DETAIL]);
        assert!(calls_to(&calls, "peer").is_empty());
    }
}
