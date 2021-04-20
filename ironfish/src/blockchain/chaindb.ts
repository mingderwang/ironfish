/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

import { Transaction } from '../primitives/transaction'
import { JsonSerializable } from '../serde'
import { BufferArrayEncoding, BufferEncoding, IDatabase, IDatabaseStore, JsonEncoding, SchemaValue, StringEncoding } from '../storage'
import { createDB } from '../storage/utils'
import { Graph } from './graph'
import {
  GraphSchema,
  HashToNextSchema,
  HeadersSchema,
  SCHEMA_VERSION,
  SequenceToHashSchema,
  TransactionsSchema,
} from './schema'

export class ChainDB<
  E,
  H,
  T extends Transaction<E, H>,
  SE extends JsonSerializable,
  SH extends JsonSerializable,
  ST
> {
  db: IDatabase
  headers: IDatabaseStore<HeadersSchema<SH>>
  transactions: IDatabaseStore<TransactionsSchema<ST>>
  sequenceToHash: IDatabaseStore<SequenceToHashSchema>
  hashToNext: IDatabaseStore<HashToNextSchema>
  graphs: IDatabaseStore<GraphSchema>

  constructor(options: { location: string }) {
    this.db = createDB({ location: options.location })

    this.headers = this.db.addStore({
      version: SCHEMA_VERSION,
      name: 'Headers',
      keyEncoding: new BufferEncoding(), // block hash
      valueEncoding: new JsonEncoding<SchemaValue<HeadersSchema<SH>>>(),
    })

    this.transactions = this.db.addStore({
      version: SCHEMA_VERSION,
      name: 'Transactions',
      keyEncoding: new BufferEncoding(), // block hash
      valueEncoding: new JsonEncoding<ST[]>(),
    })

    this.sequenceToHash = this.db.addStore({
      version: SCHEMA_VERSION,
      name: 'SequenceToHash',
      keyEncoding: new StringEncoding(),
      valueEncoding: new BufferArrayEncoding(),
    })

    this.hashToNext = this.db.addStore({
      version: SCHEMA_VERSION,
      name: 'HashToNextHash',
      keyEncoding: new BufferEncoding(),
      valueEncoding: new BufferArrayEncoding(),
    })

    this.graphs = this.db.addStore({
      version: SCHEMA_VERSION,
      name: 'Graphs',
      keyEncoding: new StringEncoding(),
      valueEncoding: new JsonEncoding<Graph>(),
    })
  }

  open(): Promise<void> {}
  close(): Promise<void> {}
}
