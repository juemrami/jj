// Copyright 2020 The Jujutsu Authors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// https://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

#![allow(missing_docs)]

use std::sync::Arc;

use pollster::FutureExt as _;

use crate::backend;
use crate::backend::BackendError;
use crate::backend::BackendResult;
use crate::backend::ChangeId;
use crate::backend::CommitId;
use crate::backend::MergedTreeId;
use crate::backend::Signature;
use crate::commit::Commit;
use crate::commit::is_backend_commit_empty;
use crate::repo::MutableRepo;
use crate::repo::Repo;
use crate::settings::JJRng;
use crate::settings::SignSettings;
use crate::settings::UserSettings;
use crate::signing::SignBehavior;
use crate::store::Store;

#[must_use]
pub struct CommitBuilder<'repo> {
    mut_repo: &'repo mut MutableRepo,
    inner: DetachedCommitBuilder,
}

impl CommitBuilder<'_> {
    /// Detaches from `&'repo mut` lifetime. The returned builder can be used in
    /// order to obtain a temporary commit object.
    pub fn detach(self) -> DetachedCommitBuilder {
        self.inner
    }

    pub fn parents(&self) -> &[CommitId] {
        self.inner.parents()
    }

    pub fn set_parents(mut self, parents: Vec<CommitId>) -> Self {
        self.inner.set_parents(parents);
        self
    }

    pub fn predecessors(&self) -> &[CommitId] {
        self.inner.predecessors()
    }

    pub fn set_predecessors(mut self, predecessors: Vec<CommitId>) -> Self {
        self.inner.set_predecessors(predecessors);
        self
    }

    pub fn tree_id(&self) -> &MergedTreeId {
        self.inner.tree_id()
    }

    pub fn set_tree_id(mut self, tree_id: MergedTreeId) -> Self {
        self.inner.set_tree_id(tree_id);
        self
    }

    /// [`Commit::is_empty()`] for the new commit.
    pub fn is_empty(&self) -> BackendResult<bool> {
        self.inner.is_empty(self.mut_repo)
    }

    pub fn change_id(&self) -> &ChangeId {
        self.inner.change_id()
    }

    pub fn set_change_id(mut self, change_id: ChangeId) -> Self {
        self.inner.set_change_id(change_id);
        self
    }

    pub fn generate_new_change_id(mut self) -> Self {
        self.inner.generate_new_change_id();
        self
    }

    pub fn description(&self) -> &str {
        self.inner.description()
    }

    pub fn set_description(mut self, description: impl Into<String>) -> Self {
        self.inner.set_description(description);
        self
    }

    pub fn author(&self) -> &Signature {
        self.inner.author()
    }

    pub fn set_author(mut self, author: Signature) -> Self {
        self.inner.set_author(author);
        self
    }

    pub fn committer(&self) -> &Signature {
        self.inner.committer()
    }

    pub fn set_committer(mut self, committer: Signature) -> Self {
        self.inner.set_committer(committer);
        self
    }

    /// [`Commit::is_discardable()`] for the new commit.
    pub fn is_discardable(&self) -> BackendResult<bool> {
        self.inner.is_discardable(self.mut_repo)
    }

    pub fn sign_settings(&self) -> &SignSettings {
        self.inner.sign_settings()
    }

    pub fn set_sign_behavior(mut self, sign_behavior: SignBehavior) -> Self {
        self.inner.set_sign_behavior(sign_behavior);
        self
    }

    pub fn set_sign_key(mut self, sign_key: String) -> Self {
        self.inner.set_sign_key(sign_key);
        self
    }

    pub fn clear_sign_key(mut self) -> Self {
        self.inner.clear_sign_key();
        self
    }

    pub fn write(self) -> BackendResult<Commit> {
        self.inner.write(self.mut_repo)
    }

    /// Records the old commit as abandoned instead of writing new commit. This
    /// is noop for the builder created by [`MutableRepo::new_commit()`].
    pub fn abandon(self) {
        self.inner.abandon(self.mut_repo);
    }
}

/// Like `CommitBuilder`, but doesn't mutably borrow `MutableRepo`.
#[derive(Debug)]
pub struct DetachedCommitBuilder {
    store: Arc<Store>,
    rng: Arc<JJRng>,
    commit: backend::Commit,
    rewrite_source: Option<Commit>,
    sign_settings: SignSettings,
}

impl DetachedCommitBuilder {
    /// Only called from [`MutableRepo::new_commit`]. Use that function instead.
    pub(crate) fn for_new_commit(
        repo: &dyn Repo,
        settings: &UserSettings,
        parents: Vec<CommitId>,
        tree_id: MergedTreeId,
    ) -> Self {
        let store = repo.store().clone();
        let signature = settings.signature();
        assert!(!parents.is_empty());
        let rng = settings.get_rng();
        let change_id = rng.new_change_id(store.change_id_length());
        let commit = backend::Commit {
            parents,
            predecessors: vec![],
            root_tree: tree_id,
            change_id,
            description: String::new(),
            author: signature.clone(),
            committer: signature,
            secure_sig: None,
        };
        DetachedCommitBuilder {
            store,
            rng,
            commit,
            rewrite_source: None,
            sign_settings: settings.sign_settings(),
        }
    }

    /// Only called from [`MutableRepo::rewrite_commit`]. Use that function
    /// instead.
    pub(crate) fn for_rewrite_from(
        repo: &dyn Repo,
        settings: &UserSettings,
        predecessor: &Commit,
    ) -> Self {
        let store = repo.store().clone();
        let mut commit = backend::Commit::clone(predecessor.store_commit());
        commit.predecessors = vec![predecessor.id().clone()];
        commit.committer = settings.signature();
        // If the user had not configured a name and email before but now they have,
        // update the author fields with the new information.
        if commit.author.name.is_empty()
            || commit.author.name == UserSettings::USER_NAME_PLACEHOLDER
        {
            commit.author.name.clone_from(&commit.committer.name);
        }
        if commit.author.email.is_empty()
            || commit.author.email == UserSettings::USER_EMAIL_PLACEHOLDER
        {
            commit.author.email.clone_from(&commit.committer.email);
        }

        // Reset author timestamp on discardable commits if the author is the
        // committer. While it's unlikely we'll have somebody else's commit
        // with no description in our repo, we'd like to be extra safe.
        if commit.author.name == commit.committer.name
            && commit.author.email == commit.committer.email
            && predecessor.is_discardable(repo).unwrap_or_default()
        {
            commit.author.timestamp = commit.committer.timestamp;
        }

        DetachedCommitBuilder {
            store,
            commit,
            rng: settings.get_rng(),
            rewrite_source: Some(predecessor.clone()),
            sign_settings: settings.sign_settings(),
        }
    }

    /// Attaches the underlying `mut_repo`.
    pub fn attach(self, mut_repo: &mut MutableRepo) -> CommitBuilder<'_> {
        assert!(Arc::ptr_eq(&self.store, mut_repo.store()));
        CommitBuilder {
            mut_repo,
            inner: self,
        }
    }

    pub fn parents(&self) -> &[CommitId] {
        &self.commit.parents
    }

    pub fn set_parents(&mut self, parents: Vec<CommitId>) -> &mut Self {
        assert!(!parents.is_empty());
        self.commit.parents = parents;
        self
    }

    pub fn predecessors(&self) -> &[CommitId] {
        &self.commit.predecessors
    }

    pub fn set_predecessors(&mut self, predecessors: Vec<CommitId>) -> &mut Self {
        self.commit.predecessors = predecessors;
        self
    }

    pub fn tree_id(&self) -> &MergedTreeId {
        &self.commit.root_tree
    }

    pub fn set_tree_id(&mut self, tree_id: MergedTreeId) -> &mut Self {
        self.commit.root_tree = tree_id;
        self
    }

    /// [`Commit::is_empty()`] for the new commit.
    pub fn is_empty(&self, repo: &dyn Repo) -> BackendResult<bool> {
        is_backend_commit_empty(repo, &self.store, &self.commit)
    }

    pub fn change_id(&self) -> &ChangeId {
        &self.commit.change_id
    }

    pub fn set_change_id(&mut self, change_id: ChangeId) -> &mut Self {
        self.commit.change_id = change_id;
        self
    }

    pub fn generate_new_change_id(&mut self) -> &mut Self {
        self.commit.change_id = self.rng.new_change_id(self.store.change_id_length());
        self
    }

    pub fn description(&self) -> &str {
        &self.commit.description
    }

    pub fn set_description(&mut self, description: impl Into<String>) -> &mut Self {
        self.commit.description = description.into();
        self
    }

    pub fn author(&self) -> &Signature {
        &self.commit.author
    }

    pub fn set_author(&mut self, author: Signature) -> &mut Self {
        self.commit.author = author;
        self
    }

    pub fn committer(&self) -> &Signature {
        &self.commit.committer
    }

    pub fn set_committer(&mut self, committer: Signature) -> &mut Self {
        self.commit.committer = committer;
        self
    }

    /// [`Commit::is_discardable()`] for the new commit.
    pub fn is_discardable(&self, repo: &dyn Repo) -> BackendResult<bool> {
        Ok(self.description().is_empty() && self.is_empty(repo)?)
    }

    pub fn sign_settings(&self) -> &SignSettings {
        &self.sign_settings
    }

    pub fn set_sign_behavior(&mut self, sign_behavior: SignBehavior) -> &mut Self {
        self.sign_settings.behavior = sign_behavior;
        self
    }

    pub fn set_sign_key(&mut self, sign_key: String) -> &mut Self {
        self.sign_settings.key = Some(sign_key);
        self
    }

    pub fn clear_sign_key(&mut self) -> &mut Self {
        self.sign_settings.key = None;
        self
    }

    /// Writes new commit and makes it visible in the `mut_repo`.
    pub fn write(self, mut_repo: &mut MutableRepo) -> BackendResult<Commit> {
        let predecessors = self.commit.predecessors.clone();
        let commit = write_to_store(&self.store, self.commit, &self.sign_settings)?;
        // FIXME: Google's index.has_id() always returns true.
        if mut_repo.is_backed_by_default_index() && mut_repo.index().has_id(commit.id()) {
            // Recording existing commit as new would create cycle in
            // predecessors/parent mappings within the current transaction, and
            // in predecessors graph globally.
            return Err(BackendError::Other(
                format!("Newly-created commit {id} already exists", id = commit.id()).into(),
            ));
        }
        mut_repo.add_head(&commit)?;
        mut_repo.set_predecessors(commit.id().clone(), predecessors);
        if let Some(rewrite_source) = self.rewrite_source {
            if rewrite_source.change_id() == commit.change_id() {
                mut_repo.set_rewritten_commit(rewrite_source.id().clone(), commit.id().clone());
            }
        }
        Ok(commit)
    }

    /// Writes new commit without making it visible in the repo.
    ///
    /// This does not consume the builder, so you can reuse the current
    /// configuration to create another commit later.
    pub fn write_hidden(&self) -> BackendResult<Commit> {
        write_to_store(&self.store, self.commit.clone(), &self.sign_settings)
    }

    /// Records the old commit as abandoned in the `mut_repo`.
    ///
    /// This is noop if there's no old commit that would be rewritten to the new
    /// commit by `write()`.
    pub fn abandon(self, mut_repo: &mut MutableRepo) {
        let commit = self.commit;
        if let Some(rewrite_source) = &self.rewrite_source {
            if rewrite_source.change_id() == &commit.change_id {
                mut_repo.record_abandoned_commit_with_parents(
                    rewrite_source.id().clone(),
                    commit.parents,
                );
            }
        }
    }
}

fn write_to_store(
    store: &Arc<Store>,
    mut commit: backend::Commit,
    sign_settings: &SignSettings,
) -> BackendResult<Commit> {
    let should_sign = store.signer().can_sign() && sign_settings.should_sign(&commit);
    let sign_fn = |data: &[u8]| store.signer().sign(data, sign_settings.key.as_deref());

    // Commit backend doesn't use secure_sig for writing and enforces it with an
    // assert, but sign_settings.should_sign check above will want to know
    // if we're rewriting a signed commit
    commit.secure_sig = None;

    store
        .write_commit(commit, should_sign.then_some(&mut &sign_fn))
        .block_on()
}
