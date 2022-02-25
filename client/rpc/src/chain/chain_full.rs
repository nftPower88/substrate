// This file is part of Substrate.

// Copyright (C) 2019-2022 Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

//! Blockchain API backend for full nodes.

use super::{
	client_err,
	error::{Error, FutureResult, Result},
	BlockStats, ChainBackend,
};
use futures::FutureExt;
use jsonrpc_pubsub::manager::SubscriptionManager;
use sc_client_api::{BlockBackend, BlockchainEvents};
use sp_api::{ApiExt, Core, ProvideRuntimeApi};
use sp_blockchain::HeaderBackend;
use sp_core::Encode;
use sp_runtime::{
	generic::{BlockId, SignedBlock},
	traits::{Block as BlockT, Header},
};
use std::{marker::PhantomData, sync::Arc};

type HasherOf<Block> = <<Block as BlockT>::Header as Header>::Hashing;

/// Blockchain API backend for full nodes. Reads all the data from local database.
pub struct FullChain<Block: BlockT, Client> {
	/// Substrate client.
	client: Arc<Client>,
	/// Current subscriptions.
	subscriptions: SubscriptionManager,
	/// phantom member to pin the block type
	_phantom: PhantomData<Block>,
}

impl<Block: BlockT, Client> FullChain<Block, Client> {
	/// Create new Chain API RPC handler.
	pub fn new(client: Arc<Client>, subscriptions: SubscriptionManager) -> Self {
		Self { client, subscriptions, _phantom: PhantomData }
	}
}

impl<Block, Client> ChainBackend<Client, Block> for FullChain<Block, Client>
where
	Block: BlockT + 'static,
	Block::Header: Unpin,
	Client: BlockBackend<Block>
		+ HeaderBackend<Block>
		+ BlockchainEvents<Block>
		+ ProvideRuntimeApi<Block>
		+ 'static,
	Client::Api: Core<Block>,
{
	fn client(&self) -> &Arc<Client> {
		&self.client
	}

	fn subscriptions(&self) -> &SubscriptionManager {
		&self.subscriptions
	}

	fn header(&self, hash: Option<Block::Hash>) -> FutureResult<Option<Block::Header>> {
		let res = self.client.header(BlockId::Hash(self.unwrap_or_best(hash))).map_err(client_err);
		async move { res }.boxed()
	}

	fn block(&self, hash: Option<Block::Hash>) -> FutureResult<Option<SignedBlock<Block>>> {
		let res = self.client.block(&BlockId::Hash(self.unwrap_or_best(hash))).map_err(client_err);
		async move { res }.boxed()
	}

	fn block_stats(&self, hash: Option<Block::Hash>) -> Result<Option<BlockStats>> {
		let block = {
			let block = self
				.client
				.block(&BlockId::Hash(self.unwrap_or_best(hash)))
				.map_err(client_err)?;
			if let Some(block) = block {
				block.block
			} else {
				return Ok(None)
			}
		};
		let parent_block = {
			let parent_hash = *block.header().parent_hash();
			let parent_block =
				self.client.block(&BlockId::Hash(parent_hash)).map_err(client_err)?;
			if let Some(parent_block) = parent_block {
				parent_block.block
			} else {
				return Ok(None)
			}
		};
		let block_len = block.encoded_size() as u64;
		let block_num_extrinsics = block.extrinsics().len() as u64;
		let pre_root = *parent_block.header().state_root();
		let parent_hash = block.header().parent_hash();
		let mut runtime_api = self.client.runtime_api();
		runtime_api.record_proof();
		runtime_api
			.execute_block(&BlockId::Hash(*parent_hash), block)
			.map_err(|err| Error::Client(Box::new(err)))?;
		let witness = runtime_api
			.extract_proof()
			.ok_or_else(|| Error::Other("No Proof was recorded".to_string()))?;
		let witness_len = witness.encoded_size() as u64;
		let witness_compact = witness
			.into_compact_proof::<HasherOf<Block>>(pre_root)
			.map_err(|err| Error::Client(Box::new(err)))?
			.encode();
		let witness_compressed = zstd::stream::encode_all(&witness_compact[..], 0)
			.map_err(|err| Error::Client(Box::new(err)))?;
		Ok(Some(BlockStats {
			witness_len,
			witness_compact_len: witness_compact.len() as u64,
			witness_compressed_len: witness_compressed.len() as u64,
			block_len,
			block_num_extrinsics,
		}))
	}
}
