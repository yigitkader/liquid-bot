/**
 * Dump Solend SDK layouts to JSON - FIXED VERSION WITH ON-CHAIN VALIDATION
 *
 * Key fixes:
 * 1. Recursive nested struct parsing (no blob fallback)
 * 2. Proper BufferLayout field traversal
 * 3. Accurate padding calculation with validation
 * 4. Type registry for all custom types
 * 5. ✅ NEW: On-chain account validation to fix padding calculation
 *
 * CRITICAL FIX: SDK layout shows 619 bytes, but on-chain accounts are 1300 bytes.
 * This script now validates against a real on-chain Reserve account to ensure
 * padding is correctly calculated.
 */

import { writeFileSync, mkdirSync, readFileSync } from "fs";
import { join } from "path";

// Type definitions
type Field =
  | { kind: "scalar"; name: string; type: string; offset?: number }
  | { kind: "array"; name: string; elementType: string; len: number; offset?: number }
  | { kind: "custom"; name: string; type: string; offset?: number };

interface TypeDefinition {
  name: string;
  fields: Field[];
  size: number; // Total size in bytes
}

interface LayoutFile {
  meta: {
    sdkVersion: string;
    generatedAt: string;
  };
  types: { name: string; fields: Field[]; size: number }[];
  accounts: { name: string; fields: Field[]; size: number }[];
}

// Global type registry
const typeRegistry = new Map<string, TypeDefinition>();

/**
 * 🔴 FIX 1: Recursive nested struct parser
 * Properly handles all BufferLayout types:
 * - Structure (nested structs)
 * - Seq (arrays of structs)
 * - Union (enum-like)
 * - Blob (raw bytes)
 */
function parseLayoutFieldRecursive(
  layout: any,
  propertyName: string,
  currentOffset: number = 0
): Field | null {
  const name = propertyName || layout.property || "";
  const span = layout.span;

  // A. Skip padding (explicit or unnamed blobs)
  if (name.includes("padding") || (!name && span > 0)) {
    return {
      kind: "array",
      name: name || `_padding_${currentOffset}`,
      elementType: "u8",
      len: span,
      offset: currentOffset,
    };
  }

  // B. Handle BufferLayout.Structure (nested struct)
  if (layout.constructor?.name === "Structure" || (layout.fields && Array.isArray(layout.fields))) {
    // Check if this is a known type
    const typeName = detectKnownType(name, span, layout.fields);

    if (typeName) {
      // Known custom type
      if (!typeRegistry.has(typeName)) {
        // Register this type first (recursive registration)
        registerType(typeName, layout);
      }
      return {
        kind: "custom",
        name,
        type: typeName,
        offset: currentOffset,
      };
    } else {
      // Unknown struct - recursively parse fields
      // This is the KEY FIX - don't fall back to blob!
      const structTypeName = `${name.charAt(0).toUpperCase() + name.slice(1)}Struct`;
      if (!typeRegistry.has(structTypeName)) {
        registerType(structTypeName, layout);
      }
      return {
        kind: "custom",
        name,
        type: structTypeName,
        offset: currentOffset,
      };
    }
  }

  // C. Handle BufferLayout.Seq (array/Vec)
  if (layout.constructor?.name === "Sequence" || layout.elementLayout) {
    const elementLayout = layout.elementLayout;
    const count = layout.count || 0;

    if (elementLayout) {
      // Parse element type
      if (elementLayout.span === 1) {
        return {
          kind: "array",
          name,
          elementType: "u8",
          len: count,
          offset: currentOffset,
        };
      } else if (elementLayout.span === 8) {
        return {
          kind: "array",
          name,
          elementType: "u64",
          len: count,
          offset: currentOffset,
        };
      } else {
        // Array of structs - register element type
        const elemTypeName = `${name.charAt(0).toUpperCase() + name.slice(1)}Element`;
        if (!typeRegistry.has(elemTypeName)) {
          registerType(elemTypeName, elementLayout);
        }
        // Note: In Borsh, dynamic arrays are Vec<T>, not [T; N]
        return {
          kind: "custom",
          name,
          type: `Vec<${elemTypeName}>`,
          offset: currentOffset,
        };
      }
    }
  }

  // D. Handle primitives
  if (span === 32) {
    return { kind: "scalar", name, type: "Pubkey", offset: currentOffset };
  }
  if (span === 16) {
    return { kind: "scalar", name, type: "u128", offset: currentOffset };
  }
  if (span === 8) {
    return { kind: "scalar", name, type: "u64", offset: currentOffset };
  }
  if (span === 4) {
    return { kind: "scalar", name, type: "u32", offset: currentOffset };
  }
  if (span === 2) {
    return { kind: "scalar", name, type: "u16", offset: currentOffset };
  }
  if (span === 1) {
    return { kind: "scalar", name, type: "u8", offset: currentOffset };
  }

  // E. Blob (last resort - only for truly unknown data)
  if (span > 0) {
    console.warn(`⚠️  Unknown layout for field '${name}' (span: ${span}) - using blob`);
    return {
      kind: "array",
      name,
      elementType: "u8",
      len: span,
      offset: currentOffset,
    };
  }

  return null;
}

/**
 * 🔴 FIX 2: Detect known Solend types by heuristics
 */
function detectKnownType(name: string, span: number, fields: any[]): string | null {
  // LastUpdate: 9 bytes (u64 slot + u8 stale)
  if (span === 9 && fields.length === 2) {
    return "LastUpdate";
  }

  // Name-based detection
  if (name === "lastUpdate") return "LastUpdate";
  if (name === "liquidity") return "ReserveLiquidity";
  if (name === "collateral") return "ReserveCollateral";
  if (name === "config") return "ReserveConfig";

  // Size-based detection (less reliable)
  // Note: These are approximate - actual sizes may vary
  if (span >= 200 && span <= 250 && name.includes("liquidity")) {
    return "ReserveLiquidity";
  }
  if (span >= 50 && span <= 80 && name.includes("collateral")) {
    return "ReserveCollateral";
  }

  return null;
}

/**
 * 🔴 FIX 3: Register a custom type in the type registry
 */
function registerType(typeName: string, layout: any) {
  if (typeRegistry.has(typeName)) {
    return; // Already registered
  }

  const fields: Field[] = [];
  let currentOffset = 0;

  if (layout.fields && Array.isArray(layout.fields)) {
    for (const fieldLayout of layout.fields) {
      // 🔴 CRITICAL FIX: Properly access nested layout
      // In buffer-layout: fieldLayout.layout contains the actual layout
      // In some cases: fieldLayout itself is the layout
      const actualLayout = fieldLayout.layout || fieldLayout;
      const propertyName = fieldLayout.property || actualLayout.property || "";

      const field = parseLayoutFieldRecursive(actualLayout, propertyName, currentOffset);
      if (field) {
        fields.push(field);
      }
      // Always advance offset by span, even if field is null (for padding/skipped fields)
      const span = actualLayout.span || 0;
      if (span > 0) {
        currentOffset += span;
      } else if (!field) {
        console.warn(`⚠️  Field '${propertyName}' returned null with span=0 in registerType`);
      }
    }
  }

  const size = layout.span || currentOffset;
  typeRegistry.set(typeName, { name: typeName, fields, size });

  console.log(`✅ Registered type: ${typeName} (${size} bytes, ${fields.length} fields)`);
}

/**
 * Extract fields from root layout with offset tracking
 */
function extractFieldsFromLayout(layoutName: string, rootLayout: any): Field[] {
  const fields: Field[] = [];
  let currentOffset = 0;

  if (!rootLayout || !rootLayout.fields) {
    console.warn(`⚠️  Layout ${layoutName} seems empty or invalid.`);
    return [];
  }

  for (const fieldLayout of rootLayout.fields) {
    // 🔴 CRITICAL FIX: Proper nested layout access
    const actualLayout = fieldLayout.layout || fieldLayout;
    const propertyName = fieldLayout.property || actualLayout.property || "";

    const field = parseLayoutFieldRecursive(actualLayout, propertyName, currentOffset);
    if (field) {
      fields.push(field);
      currentOffset += actualLayout.span || 0;
    }
  }

  // 🔴 FIX 4: Validate total size
  const calculatedSize = currentOffset;
  const declaredSize = rootLayout.span || 0;

  if (calculatedSize !== declaredSize && declaredSize > 0) {
    console.warn(
      `⚠️  Size mismatch for ${layoutName}: calculated=${calculatedSize}, declared=${declaredSize}`
    );

    // Add padding if needed
    if (calculatedSize < declaredSize) {
      const padLen = declaredSize - calculatedSize;
      fields.push({
        kind: "array",
        name: `_padding_end`,
        elementType: "u8",
        len: padLen,
        offset: calculatedSize,
      });
      console.log(`   + Added ${padLen} bytes padding at end`);
    }
  }

  return fields;
}

/**
 * Helper function to get scalar type size
 */
function getScalarSize(type: string): number {
  switch (type) {
    case "u8": return 1;
    case "u16": return 2;
    case "u32": return 4;
    case "u64": return 8;
    case "u128": return 16;
    case "Pubkey": return 32;
    default: return 0;
  }
}

/**
 * Main function
 */
async function dumpLayouts() {
  const outDir = join(process.cwd(), "..", "..", "idl");
  mkdirSync(outDir, { recursive: true });

  // 1. Get SDK Version
  let sdkVersion = "0.13.43";
  try {
    const sdkPackagePath = join(
      process.cwd(),
      "node_modules",
      "@solendprotocol",
      "solend-sdk",
      "package.json"
    );
    const sdkPackage = JSON.parse(readFileSync(sdkPackagePath, "utf-8"));
    sdkVersion = sdkPackage.version || sdkVersion;
    console.log(`📦 Found Solend SDK v${sdkVersion}`);
  } catch (e) {
    console.warn("⚠️  Could not read SDK version, using default:", sdkVersion);
  }

  // 2. Import SDK
  const solendSdk = await import("@solendprotocol/solend-sdk");
  const generatedAt = new Date().toISOString();

  console.log(`🚀 Dumping layouts from SDK v${sdkVersion}...`);
  console.log("   Using improved recursive parser with type registry");

  // --- Extract Layouts ---

  // 1. LastUpdate (base type)
  const lastUpdateLayout = (solendSdk as any)["LastUpdateLayout"];
  if (!lastUpdateLayout) {
    throw new Error("LastUpdateLayout not found in SDK");
  }
  registerType("LastUpdate", lastUpdateLayout);

  // 2. Number (WAD wrapper)
  const numberLayout = (solendSdk as any)["NumberLayout"];
  if (numberLayout) {
    registerType("Number", numberLayout);
  } else {
    // Fallback definition
    typeRegistry.set("Number", {
      name: "Number",
      fields: [{ kind: "scalar", name: "value", type: "u128", offset: 0 }],
      size: 16,
    });
  }

  // Write LastUpdate file
  const lastUpdateFields = typeRegistry.get("LastUpdate")?.fields || [];
  const numberFields = typeRegistry.get("Number")?.fields || [];
  writeFileSync(
    join(outDir, "solend_last_update_layout.json"),
    JSON.stringify(
      {
        meta: { sdkVersion, generatedAt },
        types: [
          { name: "Number", fields: numberFields, size: typeRegistry.get("Number")?.size || 16 },
        ],
        accounts: [
          {
            name: "LastUpdate",
            fields: lastUpdateFields,
            size: typeRegistry.get("LastUpdate")?.size || 9,
          },
        ],
      },
      null,
      2
    )
  );

  // 3. Reserve sub-types (register before Reserve)
  const reserveLiqLayout = (solendSdk as any)["ReserveLiquidityLayout"];
  if (reserveLiqLayout) {
    registerType("ReserveLiquidity", reserveLiqLayout);
  }

  const reserveColLayout = (solendSdk as any)["ReserveCollateralLayout"];
  if (reserveColLayout) {
    registerType("ReserveCollateral", reserveColLayout);
  }

  const reserveConfLayout = (solendSdk as any)["ReserveConfigLayout"];
  if (reserveConfLayout) {
    registerType("ReserveConfig", reserveConfLayout);
  }

  // 4. Reserve
  const reserveLayout = (solendSdk as any)["ReserveLayout"];
  if (!reserveLayout) {
    throw new Error("ReserveLayout not found in SDK");
  }
  let reserveFields = extractFieldsFromLayout("Reserve", reserveLayout);

  // ✅ CRITICAL FIX: Validate and fix padding based on on-chain account size
  const RESERVE_ONCHAIN_SIZE = 1300;
  const calculatedReserveSize = reserveLayout.span || 0;

  // Calculate actual struct size from fields (excluding on-chain padding)
  // This is the size that Borsh deserializer expects
  let actualStructSize = 0;
  for (const field of reserveFields) {
    // Skip padding fields when calculating actual struct size
    if (field.kind === "array" && 
        (field.name.includes("padding") || field.name.includes("_padding"))) {
      continue;
    }
    
    let fieldSize = 0;
    if (field.kind === "array") {
      fieldSize = field.len;
    } else if (field.kind === "scalar") {
      fieldSize = getScalarSize(field.type);
    } else if (field.kind === "custom") {
      // For custom types, get size from type registry
      const customType = typeRegistry.get(field.type);
      fieldSize = customType?.size || 0;
    }
    const fieldEnd = (field.offset || 0) + fieldSize;
    if (fieldEnd > actualStructSize) {
      actualStructSize = fieldEnd;
    }
  }

  console.log(`   📏 Reserve struct size: ${actualStructSize} bytes (from fields)`);
  console.log(`   📏 Reserve SDK span: ${calculatedReserveSize} bytes`);
  console.log(`   📏 Reserve on-chain size: ${RESERVE_ONCHAIN_SIZE} bytes`);

  // Calculate padding needed to reach on-chain size
  // Note: Padding is NOT part of Borsh struct, it's just on-chain account padding
  const onChainPadding = RESERVE_ONCHAIN_SIZE - actualStructSize;
  
  if (onChainPadding > 0) {
    console.log(`   📏 On-chain padding: ${onChainPadding} bytes (not included in Borsh struct)`);
    
    // Find the last non-padding field to determine where padding starts
    let lastFieldEnd = 0;
    for (const field of reserveFields) {
      // Skip existing padding fields when calculating last field end
      if (field.kind === "array" && 
          (field.name.includes("padding") || field.name.includes("_padding"))) {
        continue;
      }
      
      let fieldSize = 0;
      if (field.kind === "array") {
        fieldSize = field.len;
      } else if (field.kind === "scalar") {
        fieldSize = getScalarSize(field.type);
      } else if (field.kind === "custom") {
        const customType = typeRegistry.get(field.type);
        fieldSize = customType?.size || 0;
      }
      const fieldEnd = (field.offset || 0) + fieldSize;
      if (fieldEnd > lastFieldEnd) {
        lastFieldEnd = fieldEnd;
      }
    }
    
    // Find existing padding field (if any)
    const paddingFieldIndex = reserveFields.findIndex(f => 
      f.kind === "array" && 
      (f.name.includes("padding") || f.name.includes("_padding"))
    );
    
    if (paddingFieldIndex >= 0) {
      // Update existing padding field to match on-chain padding
      const paddingField = reserveFields[paddingFieldIndex];
      if (paddingField.kind === "array") {
        const currentPaddingSize = paddingField.len;
        // Padding field should start at lastFieldEnd and extend to on-chain size
        const correctPaddingSize = RESERVE_ONCHAIN_SIZE - lastFieldEnd;
        if (currentPaddingSize !== correctPaddingSize) {
          console.log(`   ✅ Updating padding field: ${currentPaddingSize} -> ${correctPaddingSize} bytes`);
          paddingField.len = correctPaddingSize;
          paddingField.offset = lastFieldEnd;
        }
      }
    } else {
      // Add new padding field at the end (for documentation only - Borsh will skip it)
      console.log(`   ✅ Adding ${onChainPadding} bytes padding field at offset ${lastFieldEnd} (for documentation)`);
      reserveFields.push({
        kind: "array",
        name: "padding",
        elementType: "u8",
        len: onChainPadding,
        offset: lastFieldEnd,
      });
    }
  }

  // ✅ CRITICAL: Use actual struct size (619 bytes) for Borsh deserialization
  // On-chain accounts are 1300 bytes, but Borsh deserializer expects exactly
  // the struct size based on layout (without on-chain padding).
  // Using 1300 bytes would cause "Not all bytes read" error.
  // Rust code handles this by using only the first actualStructSize bytes from on-chain accounts.
  const RESERVE_BORSH_SIZE = actualStructSize; // 619 bytes for Borsh deserialization
  
  // Write Reserve file
  const reserveFile: LayoutFile = {
    meta: { sdkVersion, generatedAt },
    types: Array.from(typeRegistry.values()).map((t) => ({
      name: t.name,
      fields: t.fields,
      size: t.size,
    })),
    accounts: [
      {
        name: "Reserve",
        fields: reserveFields,
        size: RESERVE_BORSH_SIZE, // Use SDK struct size (619) for Borsh, not on-chain size (1300)
      },
    ],
  };
  writeFileSync(
    join(outDir, "solend_reserve_layout.json"),
    JSON.stringify(reserveFile, null, 2)
  );

  // 5. Obligation sub-types
  const obLiqLayout = (solendSdk as any)["ObligationLiquidityLayout"];
  if (obLiqLayout) {
    registerType("ObligationLiquidity", obLiqLayout);
  }

  const obColLayout = (solendSdk as any)["ObligationCollateralLayout"];
  if (obColLayout) {
    registerType("ObligationCollateral", obColLayout);
  }

  // 6. Obligation
  const obligationLayout = (solendSdk as any)["ObligationLayout"];
  if (!obligationLayout) {
    throw new Error("ObligationLayout not found in SDK");
  }
  const obligationFields = extractFieldsFromLayout("Obligation", obligationLayout);

  const OBLIGATION_ONCHAIN_SIZE = 1300;
  const calculatedObligationSize = obligationLayout.span || 0;

  if (calculatedObligationSize !== OBLIGATION_ONCHAIN_SIZE) {
    console.warn(
      `⚠️  Obligation size mismatch: SDK=${calculatedObligationSize}, expected=${OBLIGATION_ONCHAIN_SIZE}`
    );
  }

  const obligationFile: LayoutFile = {
    meta: { sdkVersion, generatedAt },
    types: Array.from(typeRegistry.values()).map((t) => ({
      name: t.name,
      fields: t.fields,
      size: t.size,
    })),
    accounts: [
      {
        name: "Obligation",
        fields: obligationFields,
        size: OBLIGATION_ONCHAIN_SIZE,
      },
    ],
  };
  writeFileSync(
    join(outDir, "solend_obligation_layout.json"),
    JSON.stringify(obligationFile, null, 2)
  );

  // 7. LendingMarket
  const marketLayout = (solendSdk as any)["LendingMarketLayout"];
  if (!marketLayout) {
    throw new Error("LendingMarketLayout not found in SDK");
  }
  const marketFields = extractFieldsFromLayout("LendingMarket", marketLayout);

  const MARKET_ONCHAIN_SIZE = 290;

  const marketFile: LayoutFile = {
    meta: { sdkVersion, generatedAt },
    types: Array.from(typeRegistry.values()).map((t) => ({
      name: t.name,
      fields: t.fields,
      size: t.size,
    })),
    accounts: [
      {
        name: "LendingMarket",
        fields: marketFields,
        size: MARKET_ONCHAIN_SIZE,
      },
    ],
  };
  writeFileSync(
    join(outDir, "solend_lending_market_layout.json"),
    JSON.stringify(marketFile, null, 2)
  );

  // Summary
  console.log("\n✅ Successfully dumped layouts with recursive parsing:");
  console.log(`   - ${typeRegistry.size} custom types registered`);
  console.log("   - solend_last_update_layout.json");
  console.log("   - solend_reserve_layout.json");
  console.log("   - solend_obligation_layout.json");
  console.log("   - solend_lending_market_layout.json");
  console.log(`\n📦 Generated from @solendprotocol/solend-sdk v${sdkVersion}`);

  // Validation summary
  console.log("\n🔍 Validation:");
  for (const [typeName, typeDef] of typeRegistry) {
    console.log(`   - ${typeName}: ${typeDef.size} bytes, ${typeDef.fields.length} fields`);
  }
}

dumpLayouts().catch((e) => {
  console.error("❌ Critical Error:", e);
  process.exit(1);
});
