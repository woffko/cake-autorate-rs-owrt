/**
 * Minimal LuCI runtime declarations used by luci-app-cake-autorate-rs.
 * Keep this file aligned with the OpenWrt LuCI JavaScript API used by the app.
 */

export {};

declare global {

interface String {
  format(...values: unknown[]): string;
}

type LuCIChild = Node | string | number | null | undefined | LuCIChild[];
type LuCIAttributes = Record<string, unknown>;

declare function E(
  tag?: string | LuCIChild[],
  attributes?: LuCIAttributes | LuCIChild,
  children?: LuCIChild,
): HTMLElement;

declare function _(message: string): string;

interface LuCIExtendable {
  extend<T extends object>(definition: T & ThisType<T>): T;
}

interface LuCIRuntime {
  Class: LuCIExtendable;
  view: LuCIExtendable;
  env: Record<string, unknown>;
  bind<T extends (...args: any[]) => any>(
    callback: T,
    thisValue: unknown,
    ...boundArguments: unknown[]
  ): (...arguments_: Parameters<T>) => ReturnType<T>;
  resolveDefault<T>(value: PromiseLike<T> | T, fallback: T): Promise<T>;
  url(...parts: string[]): string;
}

declare const L: LuCIRuntime;

interface LuCIExecResult {
  code: number;
  stdout: string;
  stderr: string;
}

interface LuCIFileStat {
  name?: string;
  path?: string;
  type?: string;
  size?: number;
  mtime?: number;
  [key: string]: unknown;
}

interface LuCIFileSystem {
  exec(command: string, arguments_?: string[]): Promise<LuCIExecResult>;
  exec_direct(command: string, arguments_: string[], type: 'text'): Promise<string>;
  list(path: string): Promise<LuCIFileStat[]>;
  read(path: string): Promise<string>;
  read_direct(path: string, type?: string): Promise<string | ArrayBuffer>;
}

declare const fs: LuCIFileSystem;

interface LuCIPoll {
  add(callback: () => unknown | Promise<unknown>, interval?: number): void;
}

declare const poll: LuCIPoll;

interface LuCIUciSection {
  [option: string]: unknown;
  ".name": string;
  ".type": string;
}

interface LuCIUci {
  add(config: string, type: string, name?: string): string;
  callApply(timeout?: number): Promise<unknown>;
  changes(): Record<string, unknown>;
  get(config: string, section: string, option?: string): unknown;
  load(config: string): Promise<unknown>;
  save(): Promise<unknown>;
  sections(config: string, type?: string, callback?: (section: LuCIUciSection) => void): LuCIUciSection[];
  set(config: string, section: string, option: string, value: unknown): void;
  unload(config: string): void;
  unset(config: string, section: string, option: string): void;
}

declare const uci: LuCIUci;

interface LuCIUi {
  addNotification(title: string | null, content: LuCIChild, level?: string): void;
  changes: Record<string, unknown>;
  createHandlerFn<T extends (...args: any[]) => any>(
    context: unknown,
    callback: T,
    ...arguments_: unknown[]
  ): (...eventArguments: unknown[]) => ReturnType<T>;
  hideModal(): void;
  menu: { flushCache(): void };
  showModal(title: string, content: LuCIChild[]): void;
}

declare const ui: LuCIUi;

declare namespace form {
  class AbstractValue {
    constructor(...arguments_: unknown[]);
    cfgvalue(sectionId: string): unknown;
    formvalue(sectionId: string): unknown;
    depends(...arguments_: unknown[]): this;
    value(value: string, label?: string): this;
    [key: string]: any;
  }

  class Value extends AbstractValue {}
  class ListValue extends AbstractValue {}
  class DynamicList extends AbstractValue {}
  class Flag extends AbstractValue {}
  class DummyValue extends AbstractValue {}
  class Button extends AbstractValue {}

  class AbstractSection {
    option<T extends typeof AbstractValue>(
      optionType: T,
      name: string,
      title?: string,
      description?: string,
    ): InstanceType<T>;
    tab(name: string, title: string, description?: string): void;
    taboption<T extends typeof AbstractValue>(
      tab: string,
      optionType: T,
      name: string,
      title?: string,
      description?: string,
    ): InstanceType<T>;
    [key: string]: any;
  }

  class NamedSection extends AbstractSection {}
  class GridSection extends AbstractSection {}

  class Map {
    constructor(config: string, title?: string, description?: string);
    section<T extends typeof AbstractSection>(
      sectionType: T,
      ...arguments_: unknown[]
    ): InstanceType<T>;
    render(): Promise<HTMLElement>;
    save(): Promise<unknown>;
    [key: string]: any;
  }
}

interface LuCINetworkDevice {
  getName(): string;
  getI18n(): string;
  [key: string]: unknown;
}

interface LuCINetwork {
  getDevices(): Promise<LuCINetworkDevice[]>;
  getNetworks(): Promise<unknown[]>;
}

declare const network: LuCINetwork;

type LuCIRpcFunction = (...arguments_: any[]) => Promise<any>;

interface LuCIRpc {
  declare(specification: Record<string, unknown>): LuCIRpcFunction;
}

declare const rpc: LuCIRpc;

declare const widgets: {
  DeviceSelect: typeof form.Value;
};

declare const cakeUi: {
  text(value: unknown): Text;
  textElement(tag: string, attributes?: any, children?: any): HTMLElement;
  readNativeResult(arguments_: string[]): Promise<any>;
  ensureAppHeader(): void;
};
}
