# Solend Layout Dump Tool

This tool dumps Solend SDK layouts to JSON format per Structure.md section 11.

## Usage

```bash
cd tools/solend-layout-dump
npm install
npm run dump-layouts
```

This will generate layout JSON files in `../../idl/` directory:
- `solend_last_update_layout.json`
- `solend_lending_market_layout.json`
- `solend_reserve_layout.json`
- `solend_obligation_layout.json`

## Note

The current implementation is a template. For production use, you need to:
1. Inspect the actual BufferLayout structures from `@solendprotocol/solend-sdk`
2. Implement full field mapping from BufferLayout to the JSON format described in Structure.md section 11.3

See Structure.md section 11.2-11.4 for details.

