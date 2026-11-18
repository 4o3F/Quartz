import { basename, dirname, resolve } from "@std/path";

export interface Process {
  id: number;
  handle: Deno.PointerObject;
}

export interface Snapshot {
  Scenario: string;
  PrivateBytes: number;
  PrivateWS: number;
  Threads: number;
  Handles: number;
  CpuMs: number;
  IdleMs: number | null;
}

function wide(text: string): Uint16Array<ArrayBuffer> {
  const buffer = new Uint16Array(text.length + 1);
  for (let i = 0; i < text.length; i++) buffer[i] = text.charCodeAt(i);
  return buffer;
}

function wideText(buffer: Uint16Array): string {
  const end = buffer.indexOf(0);
  return new TextDecoder("utf-16le").decode(
    buffer.subarray(0, end < 0 ? buffer.length : end),
  );
}

function pointer(buffer: Uint8Array, offset = 0): Deno.PointerObject {
  const address = new DataView(buffer.buffer, buffer.byteOffset).getBigUint64(
    offset,
    true,
  );
  const value = Deno.UnsafePointer.create(address);
  if (!value) throw new Error("Windows returned a null pointer");
  return value;
}

function guid(value: string): Uint8Array<ArrayBuffer> {
  const hex = value.replaceAll("-", "");
  const result = new Uint8Array(16);
  const view = new DataView(result.buffer);
  view.setUint32(0, parseInt(hex.slice(0, 8), 16), true);
  view.setUint16(4, parseInt(hex.slice(8, 12), 16), true);
  view.setUint16(6, parseInt(hex.slice(12, 16), 16), true);
  for (let i = 8; i < 16; i++) {
    result[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return result;
}

function hresult(result: number, operation: string): void {
  if (result < 0) {
    throw new Error(`${operation}: HRESULT 0x${(result >>> 0).toString(16)}`);
  }
}

export class Windows {
  private kernel = Deno.dlopen("kernel32.dll", {
    GetLastError: { parameters: [], result: "u32" },
    CloseHandle: { parameters: ["pointer"], result: "i32" },
    OpenProcess: { parameters: ["u32", "i32", "u32"], result: "pointer" },
    CreateProcessW: {
      parameters: [
        "buffer",
        "buffer",
        "pointer",
        "pointer",
        "i32",
        "u32",
        "pointer",
        "buffer",
        "buffer",
        "buffer",
      ],
      result: "i32",
    },
    QueryFullProcessImageNameW: {
      parameters: ["pointer", "u32", "buffer", "buffer"],
      result: "i32",
    },
    CreateToolhelp32Snapshot: { parameters: ["u32", "u32"], result: "pointer" },
    Process32FirstW: { parameters: ["pointer", "buffer"], result: "i32" },
    Process32NextW: { parameters: ["pointer", "buffer"], result: "i32" },
    GetProcessHandleCount: { parameters: ["pointer", "buffer"], result: "i32" },
    GetProcessTimes: {
      parameters: ["pointer", "buffer", "buffer", "buffer", "buffer"],
      result: "i32",
    },
    GetExitCodeProcess: { parameters: ["pointer", "buffer"], result: "i32" },
    WaitForSingleObject: { parameters: ["pointer", "u32"], result: "u32" },
    TerminateProcess: { parameters: ["pointer", "u32"], result: "i32" },
  });
  private psapi = Deno.dlopen("psapi.dll", {
    GetProcessMemoryInfo: {
      parameters: ["pointer", "buffer", "u32"],
      result: "i32",
    },
    EnumProcessModulesEx: {
      parameters: ["pointer", "buffer", "u32", "buffer", "u32"],
      result: "i32",
    },
    GetModuleBaseNameW: {
      parameters: ["pointer", "pointer", "buffer", "u32"],
      result: "u32",
    },
  });
  private ntdll = Deno.dlopen("ntdll.dll", {
    NtQueryInformationProcess: {
      parameters: ["pointer", "u32", "buffer", "u32", "buffer"],
      result: "i32",
    },
  });
  private user = Deno.dlopen("user32.dll", {
    FindWindowW: { parameters: ["buffer", "buffer"], result: "pointer" },
    PostMessageW: {
      parameters: ["pointer", "u32", "usize", "isize"],
      result: "i32",
    },
    GetWindowThreadProcessId: {
      parameters: ["pointer", "buffer"],
      result: "u32",
    },
    SetThreadDpiAwarenessContext: {
      parameters: ["pointer"],
      result: "pointer",
    },
    GetWindowRect: { parameters: ["pointer", "buffer"], result: "i32" },
    PrintWindow: { parameters: ["pointer", "pointer", "u32"], result: "i32" },
    EnumWindows: { parameters: ["function", "isize"], result: "i32" },
    IsWindowVisible: { parameters: ["pointer"], result: "i32" },
    GetWindow: { parameters: ["pointer", "u32"], result: "pointer" },
  });
  private dwm = Deno.dlopen("dwmapi.dll", {
    DwmGetWindowAttribute: {
      parameters: ["pointer", "u32", "buffer", "u32"],
      result: "i32",
    },
  });
  private gdi = Deno.dlopen("gdi32.dll", {
    CreateCompatibleDC: { parameters: ["pointer"], result: "pointer" },
    CreateDIBSection: {
      parameters: ["pointer", "buffer", "u32", "buffer", "pointer", "u32"],
      result: "pointer",
    },
    SelectObject: { parameters: ["pointer", "pointer"], result: "pointer" },
    DeleteObject: { parameters: ["pointer"], result: "i32" },
    DeleteDC: { parameters: ["pointer"], result: "i32" },
  });
  private gdiplus = Deno.dlopen("gdiplus.dll", {
    GdiplusStartup: {
      parameters: ["buffer", "buffer", "pointer"],
      result: "i32",
    },
    GdiplusShutdown: { parameters: ["usize"], result: "void" },
    GdipCreateBitmapFromScan0: {
      parameters: ["i32", "i32", "i32", "i32", "buffer", "buffer"],
      result: "i32",
    },
    GdipSaveImageToFile: {
      parameters: ["pointer", "buffer", "buffer", "pointer"],
      result: "i32",
    },
    GdipDisposeImage: { parameters: ["pointer"], result: "i32" },
  });

  private require<T>(result: T, operation: string): NonNullable<T> {
    if (!result) {
      throw new Error(
        `${operation}: Win32 error ${this.kernel.symbols.GetLastError()}`,
      );
    }
    return result as NonNullable<T>;
  }

  close(): void {
    this.gdiplus.close();
    this.gdi.close();
    this.dwm.close();
    this.user.close();
    this.ntdll.close();
    this.psapi.close();
    this.kernel.close();
  }

  closeProcess(process: Process): void {
    this.kernel.symbols.CloseHandle(process.handle);
  }

  private openProcess(id: number): Process {
    // PROCESS_QUERY_INFORMATION | PROCESS_VM_READ | SYNCHRONIZE | PROCESS_TERMINATE.
    return {
      id,
      handle: this.require(
        this.kernel.symbols.OpenProcess(0x100411, 0, id),
        "OpenProcess",
      ),
    };
  }

  start(exe: string): Process {
    // x64 STARTUPINFOW is 104 bytes; PROCESS_INFORMATION is 24 bytes.
    const startup = new Uint8Array(104);
    new DataView(startup.buffer).setUint32(0, startup.length, true);
    const info = new Uint8Array(24);
    this.require(
      this.kernel.symbols.CreateProcessW(
        wide(exe),
        wide(`"${exe}"`),
        null,
        null,
        0,
        0,
        null,
        wide(dirname(exe)),
        startup,
        info,
      ),
      "CreateProcessW",
    );
    this.kernel.symbols.CloseHandle(pointer(info, 8));
    return {
      id: new DataView(info.buffer).getUint32(16, true),
      handle: pointer(info),
    };
  }

  findCore(): Deno.PointerValue {
    return this.user.symbols.FindWindowW(wide("Quartz.Core"), null);
  }

  windowProcessId(window: Deno.PointerObject): number {
    const id = new Uint32Array(1);
    this.require(
      this.user.symbols.GetWindowThreadProcessId(window, id),
      "GetWindowThreadProcessId",
    );
    return id[0];
  }

  post(window: Deno.PointerObject, message: number, data = 0): void {
    this.require(
      this.user.symbols.PostMessageW(window, message, 0n, BigInt(data)),
      "PostMessage",
    );
  }

  mainWindow(id: number): Deno.PointerValue {
    let result: Deno.PointerValue = null;
    const callback = new Deno.UnsafeCallback({
      parameters: ["pointer", "isize"],
      result: "i32",
    }, (window) => {
      const processId = new Uint32Array(1);
      if (window) this.user.symbols.GetWindowThreadProcessId(window, processId);
      if (
        window && processId[0] === id &&
        this.user.symbols.IsWindowVisible(window) &&
        !this.user.symbols.GetWindow(window, 4)
      ) {
        result = window;
        return 0;
      }
      return 1;
    });
    try {
      this.user.symbols.EnumWindows(callback.pointer, 0n);
    } finally {
      callback.close();
    }
    return result;
  }

  private processes(): { id: number; threads: number; name: string }[] {
    const snapshot = this.kernel.symbols.CreateToolhelp32Snapshot(2, 0);
    if (
      !snapshot || Deno.UnsafePointer.value(snapshot) === 0xffffffffffffffffn
    ) {
      throw new Error(
        `CreateToolhelp32Snapshot: Win32 error ${this.kernel.symbols.GetLastError()}`,
      );
    }
    // x64 PROCESSENTRY32W: pid at 8, thread count at 28, szExeFile at 44.
    const entry = new Uint8Array(568);
    const view = new DataView(entry.buffer);
    view.setUint32(0, entry.length, true);
    const result = [];
    try {
      let found = this.kernel.symbols.Process32FirstW(snapshot, entry);
      while (found) {
        result.push({
          id: view.getUint32(8, true),
          threads: view.getUint32(28, true),
          name: wideText(new Uint16Array(entry.buffer, 44, 260)),
        });
        found = this.kernel.symbols.Process32NextW(snapshot, entry);
      }
      const error = this.kernel.symbols.GetLastError();
      if (error !== 18) {
        throw new Error(`Process enumeration failed: Win32 error ${error}`);
      }
    } finally {
      this.kernel.symbols.CloseHandle(snapshot);
    }
    return result;
  }

  private imagePath(process: Process): string {
    const path = new Uint16Array(32768);
    const size = new Uint32Array([path.length]);
    this.require(
      this.kernel.symbols.QueryFullProcessImageNameW(
        process.handle,
        0,
        path,
        size,
      ),
      "QueryFullProcessImageNameW",
    );
    return wideText(path.subarray(0, size[0]));
  }

  private parentId(process: Process): number {
    // x64 PROCESS_BASIC_INFORMATION: InheritedFromUniqueProcessId is at 40.
    const info = new Uint8Array(48);
    const status = this.ntdll.symbols.NtQueryInformationProcess(
      process.handle,
      0,
      info,
      info.length,
      new Uint32Array(1),
    );
    if (status !== 0) throw new Error(`NtQueryInformationProcess: ${status}`);
    return Number(new DataView(info.buffer).getBigUint64(40, true));
  }

  matchingProcesses(exe: string, parent?: number): Process[] {
    const result: Process[] = [];
    try {
      for (const entry of this.processes()) {
        if (entry.name.toLowerCase() !== basename(exe).toLowerCase()) continue;
        let process: Process;
        try {
          process = this.openProcess(entry.id);
        } catch {
          // Processes can exit between enumeration and OpenProcess.
          continue;
        }
        let keep = false;
        try {
          keep = this.imagePath(process).toLowerCase() === exe.toLowerCase() &&
            (parent === undefined ||
              (entry.id !== parent && this.parentId(process) === parent));
          if (keep) result.push(process);
        } finally {
          if (!keep) this.closeProcess(process);
        }
      }
      return result;
    } catch (error) {
      for (const process of result) this.closeProcess(process);
      throw error;
    }
  }

  snapshot(process: Process, scenario: string): Snapshot {
    // PROCESS_MEMORY_COUNTERS_EX2 (x64): cb=0, PrivateUsage=72,
    // PrivateWorkingSetSize=80, SharedCommitUsage=88; sizeof=96.
    const memory = new Uint8Array(96);
    const view = new DataView(memory.buffer);
    view.setUint32(0, memory.length, true);
    this.require(
      this.psapi.symbols.GetProcessMemoryInfo(
        process.handle,
        memory,
        memory.length,
      ),
      "GetProcessMemoryInfo",
    );
    const handles = new Uint32Array(1);
    this.require(
      this.kernel.symbols.GetProcessHandleCount(process.handle, handles),
      "GetProcessHandleCount",
    );
    const kernel = new BigUint64Array(1);
    const user = new BigUint64Array(1);
    this.require(
      this.kernel.symbols.GetProcessTimes(
        process.handle,
        new BigUint64Array(1),
        new BigUint64Array(1),
        kernel,
        user,
      ),
      "GetProcessTimes",
    );
    const entry = this.processes().find((entry) => entry.id === process.id);
    if (!entry) throw new Error(`Process ${process.id} exited during sampling`);
    return {
      Scenario: scenario,
      PrivateBytes: Number(view.getBigUint64(72, true)),
      PrivateWS: Number(view.getBigUint64(80, true)),
      Threads: entry.threads,
      Handles: handles[0],
      CpuMs: Number(kernel[0] + user[0]) / 10000,
      IdleMs: null,
    };
  }

  modules(process: Process): string[] {
    let modules = new BigUint64Array(256);
    const needed = new Uint32Array(1);
    while (true) {
      this.require(
        this.psapi.symbols.EnumProcessModulesEx(
          process.handle,
          modules,
          modules.byteLength,
          needed,
          3,
        ),
        "EnumProcessModulesEx",
      );
      if (needed[0] <= modules.byteLength) break;
      modules = new BigUint64Array(Math.ceil(needed[0] / 8));
    }
    const names: string[] = [];
    for (const address of modules.subarray(0, needed[0] / 8)) {
      const name = new Uint16Array(32768);
      this.require(
        this.psapi.symbols.GetModuleBaseNameW(
          process.handle,
          Deno.UnsafePointer.create(address),
          name,
          name.length,
        ),
        "GetModuleBaseNameW",
      );
      names.push(wideText(name));
    }
    return [...new Set(names)].sort((a, b) =>
      a.toLowerCase().localeCompare(b.toLowerCase())
    );
  }

  wait(process: Process, milliseconds: number): boolean {
    const status = this.kernel.symbols.WaitForSingleObject(
      process.handle,
      milliseconds,
    );
    if (status === 0) return true;
    if (status === 258) return false;
    throw new Error(
      `WaitForSingleObject: Win32 error ${this.kernel.symbols.GetLastError()}`,
    );
  }

  exitCode(process: Process): number {
    const code = new Uint32Array(1);
    this.require(
      this.kernel.symbols.GetExitCodeProcess(process.handle, code),
      "GetExitCodeProcess",
    );
    return code[0];
  }

  terminate(process: Process): void {
    if (!this.wait(process, 0)) {
      this.require(
        this.kernel.symbols.TerminateProcess(process.handle, 1),
        "TerminateProcess",
      );
    }
  }

  async saveWindow(window: Deno.PointerObject, path: string): Promise<void> {
    // DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2 is the pointer constant -4.
    const previous = this.require(
      this.user.symbols.SetThreadDpiAwarenessContext(
        Deno.UnsafePointer.create(0xfffffffffffffffcn),
      ),
      "SetThreadDpiAwarenessContext",
    );
    const rect = new Int32Array(4);
    const visible = new Int32Array(4);
    try {
      this.require(
        this.user.symbols.GetWindowRect(window, rect),
        "GetWindowRect",
      );
      hresult(
        this.dwm.symbols.DwmGetWindowAttribute(
          window,
          9,
          visible,
          visible.byteLength,
        ),
        "DwmGetWindowAttribute",
      );
    } finally {
      this.require(
        this.user.symbols.SetThreadDpiAwarenessContext(previous),
        "SetThreadDpiAwarenessContext restore",
      );
    }
    const width = rect[2] - rect[0];
    const height = rect[3] - rect[1];
    const cropWidth = visible[2] - visible[0];
    const cropHeight = visible[3] - visible[1];
    const cropX = visible[0] - rect[0];
    const cropY = visible[1] - rect[1];
    if (
      width <= 0 || height <= 0 || cropWidth <= 0 || cropHeight <= 0 ||
      cropX < 0 || cropY < 0 || cropX + cropWidth > width ||
      cropY + cropHeight > height
    ) {
      throw new Error(
        "Window frame bounds cannot be cropped to the window rectangle",
      );
    }
    const dc = this.require(
      this.gdi.symbols.CreateCompatibleDC(null),
      "CreateCompatibleDC",
    );
    let bitmap: Deno.PointerValue = null;
    let oldBitmap: Deno.PointerValue = null;
    try {
      // BITMAPINFOHEADER with a top-down, uncompressed 32-bit DIB.
      const info = new Uint8Array(40);
      const header = new DataView(info.buffer);
      header.setUint32(0, 40, true);
      header.setInt32(4, width, true);
      header.setInt32(8, -height, true);
      header.setUint16(12, 1, true);
      header.setUint16(14, 32, true);
      const pixels = new Uint8Array(8);
      bitmap = this.require(
        this.gdi.symbols.CreateDIBSection(dc, info, 0, pixels, null, 0),
        "CreateDIBSection",
      );
      oldBitmap = this.require(
        this.gdi.symbols.SelectObject(dc, bitmap),
        "SelectObject",
      );
      this.require(this.user.symbols.PrintWindow(window, dc, 2), "PrintWindow");
      const full = new Uint8Array(width * height * 4);
      new Deno.UnsafePointerView(pointer(pixels)).copyInto(full);
      const crop = new Uint8Array(cropWidth * cropHeight * 4);
      for (let y = 0; y < cropHeight; y++) {
        const start = ((cropY + y) * width + cropX) * 4;
        crop.set(
          full.subarray(start, start + cropWidth * 4),
          y * cropWidth * 4,
        );
      }
      const destination = resolve(path);
      await Deno.mkdir(dirname(destination), { recursive: true });
      this.savePng(crop, cropWidth, cropHeight, destination);
    } finally {
      if (oldBitmap) this.gdi.symbols.SelectObject(dc, oldBitmap);
      if (bitmap) this.gdi.symbols.DeleteObject(bitmap);
      this.gdi.symbols.DeleteDC(dc);
    }
  }

  private savePng(
    pixels: Uint8Array,
    width: number,
    height: number,
    path: string,
  ): void {
    const token = new BigUint64Array(1);
    const startup = new Uint8Array(24);
    new DataView(startup.buffer).setUint32(0, 1, true);
    const check = (status: number, operation: string) => {
      if (status !== 0) throw new Error(`${operation}: GDI+ status ${status}`);
    };
    check(
      this.gdiplus.symbols.GdiplusStartup(token, startup, null),
      "GdiplusStartup",
    );
    let image: Deno.PointerValue = null;
    try {
      const output = new Uint8Array(8);
      // PixelFormat32bppRGB ignores the unused alpha byte written by GDI.
      check(
        this.gdiplus.symbols.GdipCreateBitmapFromScan0(
          width,
          height,
          width * 4,
          0x22009,
          pixels,
          output,
        ),
        "GdipCreateBitmapFromScan0",
      );
      image = pointer(output);
      check(
        this.gdiplus.symbols.GdipSaveImageToFile(
          image,
          wide(path),
          guid("557cf406-1a04-11d3-9a73-0000f81ef32e"),
          null,
        ),
        "GdipSaveImageToFile",
      );
    } finally {
      if (image) this.gdiplus.symbols.GdipDisposeImage(image);
      this.gdiplus.symbols.GdiplusShutdown(token[0]);
    }
  }
}

// COM vtable slots and signatures follow the Windows SDK wbemcli.h interfaces.
function method<
  const Parameters extends readonly Deno.NativeType[],
  Result extends Deno.NativeResultType,
>(
  object: Deno.PointerObject,
  slot: number,
  parameters: Parameters,
  result: Result,
): Deno.UnsafeFnPointer<{ parameters: Parameters; result: Result }> {
  const table = new Deno.UnsafePointerView(object).getPointer();
  if (!table) throw new Error("COM object has no vtable");
  const address = new Deno.UnsafePointerView(table).getPointer(slot * 8);
  if (!address) throw new Error(`COM vtable slot ${slot} is null`);
  type Signature = { parameters: Parameters; result: Result };
  return new Deno.UnsafeFnPointer<Signature>(
    address as Deno.PointerObject<Signature>,
    { parameters, result },
  );
}

function release(object: Deno.PointerValue): void {
  if (object) method(object, 2, ["pointer"], "u32").call(object);
}

export function machineMetadata(): {
  cpu: string;
  gpu: string;
  caption: string;
  build: string;
} {
  const ole = Deno.dlopen("ole32.dll", {
    CoInitializeEx: { parameters: ["pointer", "u32"], result: "i32" },
    CoInitializeSecurity: {
      parameters: [
        "pointer",
        "i32",
        "pointer",
        "pointer",
        "u32",
        "u32",
        "pointer",
        "u32",
        "pointer",
      ],
      result: "i32",
    },
    CoCreateInstance: {
      parameters: ["buffer", "pointer", "u32", "buffer", "buffer"],
      result: "i32",
    },
    CoSetProxyBlanket: {
      parameters: [
        "pointer",
        "u32",
        "u32",
        "pointer",
        "u32",
        "u32",
        "pointer",
        "u32",
      ],
      result: "i32",
    },
    CoUninitialize: { parameters: [], result: "void" },
  });
  const automation = Deno.dlopen("oleaut32.dll", {
    SysAllocString: { parameters: ["buffer"], result: "pointer" },
    SysFreeString: { parameters: ["pointer"], result: "void" },
    SysStringLen: { parameters: ["pointer"], result: "u32" },
    VariantClear: { parameters: ["buffer"], result: "i32" },
  });
  const strings: Deno.PointerObject[] = [];
  const bstr = (text: string) => {
    const value = automation.symbols.SysAllocString(wide(text));
    if (!value) throw new Error("SysAllocString failed");
    strings.push(value);
    return value;
  };
  let initialized = false;
  let locator: Deno.PointerValue = null;
  let services: Deno.PointerValue = null;
  try {
    hresult(ole.symbols.CoInitializeEx(null, 0), "CoInitializeEx");
    initialized = true;
    const security = ole.symbols.CoInitializeSecurity(
      null,
      -1,
      null,
      null,
      0,
      3,
      null,
      0,
      null,
    );
    // RPC_E_TOO_LATE means process-wide COM security has already been set.
    if ((security >>> 0) !== 0x80010119) {
      hresult(security, "CoInitializeSecurity");
    }
    const output = new Uint8Array(8);
    hresult(
      ole.symbols.CoCreateInstance(
        guid("4590f811-1d3a-11d0-891f-00aa004b2e24"),
        null,
        1,
        guid("dc12a687-737f-11cf-884d-00aa004b2e24"),
        output,
      ),
      "CoCreateInstance IWbemLocator",
    );
    locator = pointer(output);
    hresult(
      method(locator, 3, [
        "pointer",
        "pointer",
        "pointer",
        "pointer",
        "pointer",
        "i32",
        "pointer",
        "pointer",
        "buffer",
      ], "i32").call(
        locator,
        bstr("ROOT\\CIMV2"),
        null,
        null,
        null,
        0,
        null,
        null,
        output,
      ),
      "IWbemLocator.ConnectServer",
    );
    services = pointer(output);
    hresult(
      ole.symbols.CoSetProxyBlanket(services, 10, 0, null, 3, 3, null, 0),
      "CoSetProxyBlanket",
    );
    const query = (
      className: string,
      properties: string[],
    ): Record<string, string>[] => {
      const rows: Record<string, string>[] = [];
      const enumeratorOutput = new Uint8Array(8);
      hresult(
        method(services!, 20, [
          "pointer",
          "pointer",
          "pointer",
          "i32",
          "pointer",
          "buffer",
        ], "i32").call(
          services,
          bstr("WQL"),
          bstr(`SELECT ${properties.join(", ")} FROM ${className}`),
          0x30,
          null,
          enumeratorOutput,
        ),
        "IWbemServices.ExecQuery",
      );
      const enumerator = pointer(enumeratorOutput);
      try {
        while (true) {
          const objectOutput = new Uint8Array(8);
          const returned = new Uint32Array(1);
          hresult(
            method(
              enumerator,
              4,
              ["pointer", "i32", "u32", "buffer", "buffer"],
              "i32",
            ).call(enumerator, -1, 1, objectOutput, returned),
            "IEnumWbemClassObject.Next",
          );
          if (!returned[0]) break;
          const object = pointer(objectOutput);
          try {
            const row: Record<string, string> = {};
            for (const property of properties) {
              // x64 VARIANT is 24 bytes; vt is at 0, the BSTR pointer is at 8.
              const variant = new Uint8Array(24);
              try {
                hresult(
                  method(object, 4, [
                    "pointer",
                    "buffer",
                    "i32",
                    "buffer",
                    "pointer",
                    "pointer",
                  ], "i32").call(
                    object,
                    wide(property),
                    0,
                    variant,
                    null,
                    null,
                  ),
                  "IWbemClassObject.Get",
                );
                const type = new DataView(variant.buffer).getUint16(0, true);
                if (type !== 8) {
                  throw new Error(
                    `WMI ${className}.${property} is not a string (VARIANT ${type})`,
                  );
                }
                const text = pointer(variant, 8);
                const bytes = new Uint8Array(
                  automation.symbols.SysStringLen(text) * 2,
                );
                new Deno.UnsafePointerView(text).copyInto(bytes);
                row[property] = new TextDecoder("utf-16le").decode(bytes);
              } finally {
                automation.symbols.VariantClear(variant);
              }
            }
            rows.push(row);
          } finally {
            release(object);
          }
        }
      } finally {
        release(enumerator);
      }
      return rows;
    };
    const cpu = query("Win32_Processor", ["Name"]).map((row) => row.Name).join(
      ", ",
    );
    const gpu = query("Win32_VideoController", ["Name"]).map((row) => row.Name)
      .join(", ");
    const os = query("Win32_OperatingSystem", ["Caption", "BuildNumber"])[0];
    if (!os) throw new Error("WMI returned no operating system");
    return { cpu, gpu, caption: os.Caption, build: os.BuildNumber };
  } finally {
    release(services);
    release(locator);
    for (const value of strings) automation.symbols.SysFreeString(value);
    if (initialized) ole.symbols.CoUninitialize();
    automation.close();
    ole.close();
  }
}
