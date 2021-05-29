/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */
import { BufferMap } from 'buffer-map'
import { Assert } from './assert'
import { Mutex } from './mutex'

type HandlerFn<TResult> = () => Promise<TResult>

export class Lock<T extends Buffer | string> {
  locks: Map<T | null, Mutex>
  customMap: typeof Map | typeof BufferMap

  constructor(customMap: typeof Map | typeof BufferMap = Map) {
    this.locks = new Map<T | null, Mutex>()
    this.customMap = customMap
  }

  getLock(key: T | null = null): Mutex {
    const lock = this.locks.get(key) || new Mutex()
    this.locks.set(key, lock)
    return lock
  }

  async run<TResult>(run: HandlerFn<TResult>): Promise<TResult>
  async run<TResult>(key: T | null, run: HandlerFn<TResult>): Promise<TResult>
  async run<TResult>(key: T | null | HandlerFn<TResult>, run?: HandlerFn<TResult>): Promise<TResult> {
    if (typeof key === 'function') {
      run = key
      key = null
    }

    Assert.isNotUndefined(run)
    const lock = this.getLock(key)
    return lock.run(run)
  }
}
