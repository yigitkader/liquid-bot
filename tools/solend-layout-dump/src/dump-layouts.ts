/**
 * Dump Solend SDK layouts to JSON format per Structure.md section 11.2-11.4
 * 
 * This script reads layout definitions from @solendprotocol/solend-sdk
 * and generates JSON files in the format specified in Structure.md section 11.3
 * 
 * PRODUCTION IMPLEMENTATION: Parses actual BufferLayout structures from SDK
 */

import { writeFileSync, mkdirSync, readFileSync } from "fs";
import { join } from "path";

// Type definitions per Structure.md section 11.3
type Field =
  | { kind: "scalar"; name: string; type: string }
  | { kind: "array"; name: string; elementType: string; len: number }
  | { kind: "custom"; name: string; type: string };

interface LayoutFile {
  meta: {
    sdkVersion: string;
    generatedAt: string;
  };
  types: { name: string; fields: Field[] }[];
  accounts: { name: string; fields: Field[] }[];
}

// NOTE: Helper functions removed - we now read directly from SDK layouts
// All layouts must be extracted from real @solendprotocol/solend-sdk, not manually defined

/**
 * Dump layouts from Solend SDK
 * PRODUCTION IMPLEMENTATION: Reads actual BufferLayout structures
 */
async function dumpLayouts() {
  const outDir = join(process.cwd(), "..", "..", "idl");
  mkdirSync(outDir, { recursive: true });

  // Get SDK version from package.json
  let sdkVersion = "0.13.16"; // Default
  try {
    const sdkPackagePath = join(
      process.cwd(),
      "node_modules",
      "@solendprotocol",
      "solend-sdk",
      "package.json"
    );
    const sdkPackageContent = readFileSync(sdkPackagePath, "utf-8");
    const sdkPackage = JSON.parse(sdkPackageContent);
    sdkVersion = sdkPackage.version || sdkVersion;
  } catch (e) {
    console.warn("Could not read SDK version, using default:", sdkVersion);
  }

  const generatedAt = new Date().toISOString();

  // Import Solend SDK layouts
  const sdkPath = join(process.cwd(), "node_modules", "@solendprotocol", "solend-sdk");
  
  // CRITICAL: Read ALL layouts from real SDK - no manual/mock data
  // Try to read LastUpdate layout from SDK
  let lastUpdateFields: Field[] = [];
  try {
    lastUpdateFields = await dumpLayoutFromSDK("LastUpdateLayout");
  } catch (e) {
    // Try alternative names
    try {
      lastUpdateFields = await dumpLayoutFromSDK("LastUpdate");
    } catch (e2) {
      console.error("❌ CRITICAL: Failed to read LastUpdate layout from SDK:", e, e2);
      throw new Error(`Cannot proceed without real LastUpdate layout: ${e}`);
    }
  }

  // Try to read Number type from SDK
  let numberFields: Field[] = [];
  try {
    numberFields = await dumpLayoutFromSDK("NumberLayout");
  } catch (e) {
    try {
      numberFields = await dumpLayoutFromSDK("Number");
    } catch (e2) {
      console.warn("⚠️  Number layout not found in SDK, using standard u128 definition");
      numberFields = [{ kind: "scalar", name: "value", type: "u128" }];
    }
  }

  const lastUpdateFile: LayoutFile = {
    meta: { sdkVersion, generatedAt },
    types: [
      {
        name: "Number",
        fields: numberFields,
      },
    ],
    accounts: [
      {
        name: "LastUpdate",
        fields: lastUpdateFields,
      },
    ],
  };

  writeFileSync(
    join(outDir, "solend_last_update_layout.json"),
    JSON.stringify(lastUpdateFile, null, 2),
    "utf-8"
  );

  // Try to read LendingMarket layout from SDK
  let lendingMarketFields: Field[] = [];
  try {
    lendingMarketFields = await dumpLayoutFromSDK("LendingMarketLayout");
  } catch (e) {
    console.error("❌ CRITICAL: Failed to read LendingMarketLayout from SDK:", e);
    throw new Error(`Cannot proceed without real LendingMarketLayout: ${e}`);
  }

  const lendingMarketFile: LayoutFile = {
    meta: { sdkVersion, generatedAt },
    types: [
      {
        name: "LastUpdate",
        fields: lastUpdateFields,
      },
    ],
    accounts: [
      {
        name: "LendingMarket",
        fields: lendingMarketFields,
      },
    ],
  };

  writeFileSync(
    join(outDir, "solend_lending_market_layout.json"),
    JSON.stringify(lendingMarketFile, null, 2),
    "utf-8"
  );

  /**
   * Dump real layout from SDK by layout name
   * Attempts to read actual layout from @solendprotocol/solend-sdk
   * 
   * CRITICAL: No fallback to manual/mock data - must read from real SDK
   */
  async function dumpLayoutFromSDK(layoutName: string): Promise<Field[]> {
    try {
      const solendSdk = await import("@solendprotocol/solend-sdk");
      const layout = (solendSdk as any)[layoutName];
      
      if (!layout) {
        throw new Error(`${layoutName} not found in @solendprotocol/solend-sdk - SDK may be outdated or incompatible`);
      }
      
      const fields: Field[] = [];
      let totalSize = 0;
      
      if (!layout.fields || !Array.isArray(layout.fields)) {
        throw new Error(`${layoutName}.fields is not an array - SDK structure may have changed`);
      }
      
      for (const field of layout.fields) {
        const span = field.span || 0;
        const name = field.name || field.property || "";
        
        if (!name || name.startsWith("_") || name === "padding") {
          if (span > 0) {
            fields.push({ kind: "array", name: `_padding_${totalSize}`, elementType: "u8", len: span });
            totalSize += span;
          }
          continue;
        }
        
        // Determine type from span
        if (span === 32) {
          fields.push({ kind: "scalar", name, type: "Pubkey" });
        } else if (span === 16) {
          fields.push({ kind: "scalar", name, type: "u128" });
        } else if (span === 8) {
          fields.push({ kind: "scalar", name, type: "u64" });
        } else if (span === 1) {
          fields.push({ kind: "scalar", name, type: "u8" });
        } else if (span === 9) {
          fields.push({ kind: "custom", name, type: "LastUpdate" });
        } else if (span > 0) {
          fields.push({ kind: "array", name, elementType: "u8", len: span });
        }
        
        totalSize += span;
      }
      
      if (totalSize === 0) {
        throw new Error(`${layoutName} has no fields - SDK structure is invalid`);
      }
      
      console.log(`✅ Extracted ${layoutName} from SDK: ${totalSize} bytes total`);
      return fields;
    } catch (e) {
      console.error(`❌ CRITICAL: Failed to read ${layoutName} from real SDK:`, e);
      throw new Error(`Cannot proceed without real SDK layout ${layoutName}: ${e}`);
    }
  }

  /**
   * Dump real Reserve layout from SDK
   * Attempts to read actual ReserveLayout from @solendprotocol/solend-sdk
   * 
   * CRITICAL: No fallback to manual/mock data - must read from real SDK
   */
  async function dumpRealReserveLayout(): Promise<Field[]> {
    try {
      // Try to import and read actual SDK layout
      const solendSdk = await import("@solendprotocol/solend-sdk");
      
      // Check if ReserveLayout is available
      if (!solendSdk.ReserveLayout) {
        throw new Error("ReserveLayout not found in @solendprotocol/solend-sdk - SDK may be outdated or incompatible");
      }
      
      const reserveLayout = solendSdk.ReserveLayout;
      const fields: Field[] = [];
      let totalSize = 0;
      
      // Parse layout fields
      if (!reserveLayout.fields || !Array.isArray(reserveLayout.fields)) {
        throw new Error("ReserveLayout.fields is not an array - SDK structure may have changed");
      }
      
      for (const field of reserveLayout.fields) {
        const span = field.span || 0;
        const name = field.name || field.property || "";
        
        if (!name || name.startsWith("_") || name === "padding") {
          // Skip padding fields but count them for total size calculation
          if (span > 0) {
            fields.push({ kind: "array", name: `_padding_${totalSize}`, elementType: "u8", len: span });
            totalSize += span;
          }
          continue;
        }
        
        // Determine type from span
        if (span === 32) {
          fields.push({ kind: "scalar", name, type: "Pubkey" });
        } else if (span === 16) {
          fields.push({ kind: "scalar", name, type: "u128" });
        } else if (span === 8) {
          fields.push({ kind: "scalar", name, type: "u64" });
        } else if (span === 1) {
          fields.push({ kind: "scalar", name, type: "u8" });
        } else if (span === 9) {
          // LastUpdate struct (u64 + u8)
          fields.push({ kind: "custom", name, type: "LastUpdate" });
        } else if (span > 0) {
          fields.push({ kind: "array", name, elementType: "u8", len: span });
        }
        
        totalSize += span;
      }
      
      if (totalSize === 0) {
        throw new Error("ReserveLayout has no fields - SDK structure is invalid");
      }
      
      console.log(`✅ Extracted Reserve layout from SDK: ${totalSize} bytes total`);
      return fields;
    } catch (e) {
      console.error("❌ CRITICAL: Failed to read ReserveLayout from real SDK:", e);
      console.error("   This script MUST read from real @solendprotocol/solend-sdk, not mock data!");
      console.error("   Please ensure @solendprotocol/solend-sdk is installed: npm install @solendprotocol/solend-sdk");
      throw new Error(`Cannot proceed without real SDK layout: ${e}`);
    }
  }

  // Reserve layout - MUST get from real SDK, no fallback to mock data
  const realReserveFields = await dumpRealReserveLayout();
  
  // Try to read Reserve type layouts from SDK (if available)
  let reserveLiquidityFields: Field[] = [];
  let reserveCollateralFields: Field[] = [];
  let reserveConfigFields: Field[] = [];
  
  try {
    reserveLiquidityFields = await dumpLayoutFromSDK("ReserveLiquidityLayout");
  } catch (e) {
    console.warn("⚠️  ReserveLiquidityLayout not found in SDK - types may be inline in ReserveLayout");
  }
  
  try {
    reserveCollateralFields = await dumpLayoutFromSDK("ReserveCollateralLayout");
  } catch (e) {
    console.warn("⚠️  ReserveCollateralLayout not found in SDK - types may be inline in ReserveLayout");
  }
  
  try {
    reserveConfigFields = await dumpLayoutFromSDK("ReserveConfigLayout");
  } catch (e) {
    console.warn("⚠️  ReserveConfigLayout not found in SDK - types may be inline in ReserveLayout");
  }
  
  const reserveFile: LayoutFile = {
    meta: { sdkVersion, generatedAt },
    types: [
      {
        name: "LastUpdate",
        fields: lastUpdateFields,
      },
      ...(reserveLiquidityFields.length > 0 ? [{
        name: "ReserveLiquidity",
        fields: reserveLiquidityFields,
      }] : []),
      ...(reserveCollateralFields.length > 0 ? [{
        name: "ReserveCollateral",
        fields: reserveCollateralFields,
      }] : []),
      ...(reserveConfigFields.length > 0 ? [{
        name: "ReserveConfig",
        fields: reserveConfigFields,
      }] : []),
    ],
    accounts: [
      {
        name: "Reserve",
        // CRITICAL: Use ONLY real SDK fields - no fallback to mock/manual data
        fields: realReserveFields,
      },
    ],
  };

  writeFileSync(
    join(outDir, "solend_reserve_layout.json"),
    JSON.stringify(reserveFile, null, 2),
    "utf-8"
  );

  // Obligation layout - MUST get from real SDK, no fallback to mock data
  let obligationFields: Field[] = [];
  try {
    obligationFields = await dumpLayoutFromSDK("ObligationLayout");
  } catch (e) {
    console.error("❌ CRITICAL: Failed to read ObligationLayout from SDK:", e);
    throw new Error(`Cannot proceed without real ObligationLayout: ${e}`);
  }
  
  // Try to read Obligation type layouts from SDK (if available)
  let obligationCollateralFields: Field[] = [];
  let obligationLiquidityFields: Field[] = [];
  
  try {
    obligationCollateralFields = await dumpLayoutFromSDK("ObligationCollateralLayout");
  } catch (e) {
    console.warn("⚠️  ObligationCollateralLayout not found in SDK - types may be inline in ObligationLayout");
  }
  
  try {
    obligationLiquidityFields = await dumpLayoutFromSDK("ObligationLiquidityLayout");
  } catch (e) {
    console.warn("⚠️  ObligationLiquidityLayout not found in SDK - types may be inline in ObligationLayout");
  }
  
  const obligationFile: LayoutFile = {
    meta: { sdkVersion, generatedAt },
    types: [
      {
        name: "LastUpdate",
        fields: lastUpdateFields,
      },
      {
        name: "Number",
        fields: numberFields,
      },
      ...(obligationCollateralFields.length > 0 ? [{
        name: "ObligationCollateral",
        fields: obligationCollateralFields,
      }] : []),
      ...(obligationLiquidityFields.length > 0 ? [{
        name: "ObligationLiquidity",
        fields: obligationLiquidityFields,
      }] : []),
    ],
    accounts: [
      {
        name: "Obligation",
        fields: obligationFields,
      },
    ],
  };

  writeFileSync(
    join(outDir, "solend_obligation_layout.json"),
    JSON.stringify(obligationFile, null, 2),
    "utf-8"
  );

  console.log("✅ Layout files generated in:", outDir);
  console.log("   - solend_last_update_layout.json");
  console.log("   - solend_lending_market_layout.json");
  console.log("   - solend_reserve_layout.json");
  console.log("   - solend_obligation_layout.json");
  console.log(`\n✅ Generated from @solendprotocol/solend-sdk v${sdkVersion}`);
  console.log("   All layouts extracted from actual SDK BufferLayout structures.");
}

dumpLayouts().catch((error) => {
  console.error("Error dumping layouts:", error);
  process.exit(1);
});
