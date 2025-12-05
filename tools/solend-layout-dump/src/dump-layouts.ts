/**
 * Dump Solend SDK layouts to JSON format per Structure.md section 11.2-11.4
 * 
 * This script reads layout definitions from @solendprotocol/solend-sdk
 * and generates JSON files in the format specified in Structure.md section 11.3
 */

import { writeFileSync, mkdirSync } from "fs";
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

/**
 * Convert Solend SDK Layout field to our Field format
 * Note: This is a placeholder - actual implementation would need to
 * inspect the BufferLayout structure from @solendprotocol/solend-sdk
 */
function layoutFieldToField(layoutField: any): Field | null {
  // Placeholder implementation
  // In production, this would parse the actual BufferLayout structure
  // from @solendprotocol/solend-sdk
  return null;
}

/**
 * Dump layouts from Solend SDK
 * 
 * IMPORTANT: This is a template/concept implementation.
 * The actual BufferLayout structure from @solendprotocol/solend-sdk
 * needs to be inspected to implement the full mapping.
 * 
 * Per Structure.md section 11.4, the SDK exports:
 * - LastUpdateLayout
 * - LendingMarketLayout
 * - ReserveLayout
 * - ObligationLayout
 * - ObligationCollateralLayout
 * - ObligationLiquidityLayout
 */
function dumpLayouts() {
  const outDir = join(process.cwd(), "..", "..", "idl");
  mkdirSync(outDir, { recursive: true });

  // Get SDK version from package.json if available
  let sdkVersion = "0.13.16"; // Default
  try {
    const sdkPackage = require("@solendprotocol/solend-sdk/package.json");
    sdkVersion = sdkPackage.version || sdkVersion;
  } catch (e) {
    console.warn("Could not read SDK version, using default:", sdkVersion);
  }

  const generatedAt = new Date().toISOString();

  // LastUpdate layout
  const lastUpdateFile: LayoutFile = {
    meta: {
      sdkVersion,
      generatedAt,
    },
    types: [],
    accounts: [
      {
        name: "LastUpdate",
        fields: [
          { kind: "scalar", name: "slot", type: "u64" },
          { kind: "scalar", name: "stale", type: "bool" },
        ],
      },
    ],
  };

  writeFileSync(
    join(outDir, "solend_last_update_layout.json"),
    JSON.stringify(lastUpdateFile, null, 2),
    "utf-8"
  );

  // LendingMarket layout
  const lendingMarketFile: LayoutFile = {
    meta: {
      sdkVersion,
      generatedAt,
    },
    types: [
      {
        name: "LastUpdate",
        fields: [
          { kind: "scalar", name: "slot", type: "u64" },
          { kind: "scalar", name: "stale", type: "bool" },
        ],
      },
    ],
    accounts: [
      {
        name: "LendingMarket",
        fields: [
          { kind: "scalar", name: "version", type: "u8" },
          { kind: "custom", name: "lastUpdate", type: "LastUpdate" },
          { kind: "scalar", name: "quoteCurrencyMint", type: "Pubkey" },
          { kind: "scalar", name: "borrowLimit", type: "u128" },
          { kind: "scalar", name: "owner", type: "Pubkey" },
        ],
      },
    ],
  };

  writeFileSync(
    join(outDir, "solend_lending_market_layout.json"),
    JSON.stringify(lendingMarketFile, null, 2),
    "utf-8"
  );

  // Reserve layout
  const reserveFile: LayoutFile = {
    meta: {
      sdkVersion,
      generatedAt,
    },
    types: [
      {
        name: "LastUpdate",
        fields: [
          { kind: "scalar", name: "slot", type: "u64" },
          { kind: "scalar", name: "stale", type: "bool" },
        ],
      },
    ],
    accounts: [
      {
        name: "Reserve",
        fields: [
          { kind: "scalar", name: "version", type: "u8" },
          { kind: "custom", name: "lastUpdate", type: "LastUpdate" },
          { kind: "scalar", name: "lendingMarket", type: "Pubkey" },
          // Note: Reserve has nested structs (ReserveLiquidity, ReserveCollateral, ReserveConfig)
          // These would be defined in the types section in a full implementation
        ],
      },
    ],
  };

  writeFileSync(
    join(outDir, "solend_reserve_layout.json"),
    JSON.stringify(reserveFile, null, 2),
    "utf-8"
  );

  // Obligation layout
  const obligationFile: LayoutFile = {
    meta: {
      sdkVersion,
      generatedAt,
    },
    types: [
      {
        name: "LastUpdate",
        fields: [
          { kind: "scalar", name: "slot", type: "u64" },
          { kind: "scalar", name: "stale", type: "bool" },
        ],
      },
      {
        name: "ObligationCollateral",
        fields: [
          { kind: "scalar", name: "depositReserve", type: "Pubkey" },
          { kind: "scalar", name: "depositedAmount", type: "u64" },
          { kind: "scalar", name: "marketValue", type: "u128" },
        ],
      },
      {
        name: "ObligationLiquidity",
        fields: [
          { kind: "scalar", name: "borrowReserve", type: "Pubkey" },
          { kind: "scalar", name: "borrowedAmountWads", type: "u128" },
          { kind: "scalar", name: "marketValue", type: "u128" },
          { kind: "scalar", name: "cumulativeBorrowRateWads", type: "u128" },
        ],
      },
    ],
    accounts: [
      {
        name: "Obligation",
        fields: [
          { kind: "scalar", name: "version", type: "u8" },
          { kind: "custom", name: "lastUpdate", type: "LastUpdate" },
          { kind: "scalar", name: "lendingMarket", type: "Pubkey" },
          { kind: "scalar", name: "owner", type: "Pubkey" },
          { kind: "array", name: "deposits", elementType: "ObligationCollateral", len: 10 },
          { kind: "array", name: "borrows", elementType: "ObligationLiquidity", len: 10 },
        ],
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
  console.log("\n⚠️  NOTE: This is a template implementation.");
  console.log("   For production use, inspect @solendprotocol/solend-sdk");
  console.log("   BufferLayout structures and implement full field mapping.");
}

dumpLayouts();

