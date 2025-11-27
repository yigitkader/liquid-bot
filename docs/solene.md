Kısa cevap: Bu değerleri tahmin ederek bulmaya çalışmana gerek yok; Solend’in **on-chain programı ve TS SDK’sı tamamen açık kaynak** ve hepsinin layout’ları orada net bir şekilde yazıyor. Sorun, doğru yeri bulmakta 🙂 Aşağıya adım adım “oracleOption / oracle enum / offset” gibi şeyleri *kesin* olarak nasıl çıkaracağını yazıyorum.

---

## 1. Gerçek Solend program kodu nerede?

### a) On-chain programın Rust kaynağı

Solend lending programı, `solendprotocol/solana-program-library` fork’unda: ([Solend Developer Portal][1])

* **Programın kendisi (Rust):**

    * Crate: `solend-token-lending` ([Docs.rs][2])
    * Kaynak dosyalar:

        * `src/state/reserve.rs`
        * `src/state/obligation.rs`
        * `src/instruction.rs`
        * `src/processor.rs`

Docs.rs üzerinden direkt görebiliyorsun: ([Docs.rs][3])

* Kaynak root:

    * `https://docs.rs/crate/solend-token-lending/latest/source/`
* Oradan:

    * `src/state/reserve.rs` → reserve layout + config + oracle alanları
    * `src/state/obligation.rs` → obligation layout
    * `src/state/mod.rs` → enum’lar, ortak tipler
    * `program-id.md` → program id (`LendZq...`) ([Docs.rs][4])

Buradaki kod **on-chain’de çalışan programın bire bir karşılığı**; yani gerçek offset’ler, field sıraları, enum numerical değerleri burada.

---

## 2. “oracleOption” / enum / offset tam olarak nasıl bulunur?

### Adım 1 – İlgili struct’ı bul

1. `reserve.rs` içinde:

    * `pub struct Reserve { ... }`
    * `pub struct ReserveConfig { ... }`
2. `obligation.rs` içinde:

    * `pub struct Obligation { ... }`
    * `pub struct ObligationCollateral / ObligationLiquidity { ... }`

Bu struct’ların içinde oracle ile ilgili alanları göreceksin (örneğin pyth/switchboard oracle, oracle source vs). Bunlar bazen `COption<Pubkey>`, bazen `u8` veya `u32` flag olabilir — ama hepsi struct’ın içinde açık.

### Adım 2 – Asıl layout: `Pack` implementasyonunu oku

Solend, Borsh değil; kendi `Pack` implementasyonunu kullanıyor. `reserve.rs` içinde:

* `impl Pack for Reserve { const LEN: ...; fn unpack_from_slice(...) { ... } fn pack_into_slice(...) { ... } }`

Bu fonksiyonda:

* `let input = array_ref![src, 0, LEN];`
* `let (field1, field2, ... , oracle_option_bytes, ...) = array_refs![input, ..., ..., N, ...];`
* Sonra `let oracle_option = <bir şey>::unpack(oracle_option_bytes);` gibi.

Bu bölüm:

* **Hangi field kaç byte** (N)
* **Hangi sırada**
* `oracleOption`’ın *gerçek* tipi (`u8`, `u32`, `COption<Pubkey>` vs)
  olduğunu %100 net gösteriyor.

> Yani offset hesabını kendin yapmak yerine, `array_refs![...]` içinde `oracleOption`’a denk gelen slice’ın uzunluğuna bak → offset + uzunluk = tam layout.

Aynı mantık obligation için `obligation.rs` içindeki `impl Pack for Obligation`’da geçerli.

### Adım 3 – Enum değerlerini bul (0 mı, 1 mi, başka mı?)

Oracle türleri ya da benzeri seçenekler için:

1. `state` modülünde (`src/state/mod.rs` veya `reserve.rs`) `enum` tanımını ara:

    * Ör: `pub enum OracleType { ... }` veya benzeri bir enum.
2. Enum genelde `#[repr(u8)]` veya `#[repr(u32)]` ya da `FromPrimitive` ile kullanılır:

    * `#[repr(u8)]` varsa: variant sırası → sayısal değerler (0,1,2,...)
    * Ya da `impl From<u8> for OracleType` / `FromPrimitive` tarzı mapping vardır.
3. `unpack` kısmında şöyle bir şey göreceksin:

    * `let oracle_type = OracleType::try_from_primitive(oracle_type_u8)?;`
    * Veya `OracleType::from(oracle_type_u8)` vs.

Bu kod, **real numeric value**’ları veriyor. TS tarafında da aynı sayıları kullanman gerekiyor.

---

## 3. “Solend SDK’daki layout comment, parse edilmiyor” problemi

TS SDK içinde (yeni monorepo `public/solend-sdk`) bazı alanlar için gerçekten “commented out / TODO” bırakılmış olabilir. Yani:

* SDK’da `oracleOption` gibi alanlar `layout`’ta **yorum satırı** olabilir.
* Bu, **on-chain’de yok** anlamına gelmiyor; sadece TS SDK henüz parse etmiyor.

Bu durumda yapman gereken:

1. Rust tarafındaki `Pack` / struct layout’ını **kaynak** kabul et.
2. Kendi TS parser’ında:

    * `buffer`’ı al,
    * Rust’taki sıralamaya göre slice et,
    * Oracle alanını kendin decode et (`u8`, `u32`, `COption` pattern’i vs).

İstersen, TS SDK’nın eski (read-only) repo’suna da bakabilirsin; orada `parseReserve` / `parseObligation` fonksiyonları var ve çoğu layout oradan da görülebiliyor.([GitHub][5])

---

## 4. Offset’in gerçekten doğru olduğundan nasıl emin olursun?

Manuel hesap yerine şu yaklaşımı kullanmak daha güvenli:

1. **Resmi TS SDK ile** bir reserve/obligation account’u parse et:

    * `parseReserve(pubkey, accountInfo)` / `parseObligation(...)` çağır.
    * Sonucu JSON logla.
2. Aynı account’un `accountInfo.data` buffer’ını al.
3. Kendi parser’ının çıktısını SDK’nın çıktısıyla kıyasla:

    * Her field eşleşiyor mu?
    * Özellikle oracle ile ilgili alanlarda farklılık var mı?

Eğer SDK oracle alanını parse etmiyorsa:

* Rust `Pack` layout’ına bak → kendi custom parser’ınla sadece o alanı ekle.
* Birkaç farklı reserve/obligation account üzerinde cross-check yap.

---

## 5. “Solend ekibine sormak” ne zaman mantıklı?

Şu iki durumda Discord’a yazmak gerçekten değerli:

1. **Yeni V2 / yeni branch** kullanıyorsan:

    * Program ID `So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo` olan versiyon için layout’ta değişiklikler olabilir. ([Solana Stack Exchange][6])
    * Bu durumda hangi branch’in production’da olduğunu (mainnet) sorup ona göre kodu okumalısın.
2. **Kodda yorum var ama anlamı belirsizse:**

    * Örn: `oracleOption` gibi flag’lerin semantiği (hangi bit ne demek?) dokümante değilse, en doğru kaynak ekip.

Discord linki dev portalda var: `#dev-support` kanalı.([Solend Developer Portal][7])

Mesaj örneği:

> “Hey, I’m parsing the on-chain `Reserve`/`Obligation` accounts directly. In `reserve.rs`, there is the `oracleOption` / `OracleType` field at offset X. Can you confirm the exact enum values and serialization (u8/u32/COption) for mainnet program So1endDq...?”

---

## 6. Özet – Tek satırda cevap

* **Değerleri “bulmak” = Rust kaynakta `state/*.rs` + `Pack` implementasyonundaki layout’u okumak.**
* Offset, format (COption vs u32), enum numeric değerleri hepsi orada **kesin**; TS SDK’daki yorumlar sadece “henüz parse etmedik” demek, “on-chain’de yok” demek değil.

İstersen, bir sonraki adımda senin elindeki **TS/Binary parser kodunu** buraya yapıştır; direkt o kodun üzerinden gidip hangi field’ların yanlış/belirsiz olduğunu tek tek işaretleyelim.

[1]: https://dev.solend.fi/docs/protocol/ "Solend lending program | Solend Developer Portal"
[2]: https://docs.rs/crate/solend-token-lending/latest?utm_source=chatgpt.com "solend-token-lending 0.1.1"
[3]: https://docs.rs/crate/solend-token-lending/latest/source/ "solend-token-lending 0.1.1 - Docs.rs"
[4]: https://docs.rs/crate/solend-token-lending/latest/source/program-id.md "solend-token-lending 0.1.1 - Docs.rs"
[5]: https://github.com/solendprotocol/solend-sdk?utm_source=chatgpt.com "solendprotocol/solend-sdk"
[6]: https://solana.stackexchange.com/questions/20790/instruction-0-instruction-expected-an-executable-account?utm_source=chatgpt.com "Instruction 0: instruction expected an executable account"
[7]: https://dev.solend.fi/?utm_source=chatgpt.com "Solend Developer Portal | Solend Developer Portal"
