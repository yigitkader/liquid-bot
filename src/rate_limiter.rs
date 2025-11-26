use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// Token Bucket Rate Limiter - RPC çağrılarını sınırlandırır
/// 
/// Token bucket algoritması:
/// - Belirli bir kapasiteye sahip bir "bucket" (token deposu)
/// - Her saniye belirli sayıda token eklenir (refill rate)
/// - Her request bir token tüketir
/// - Token yoksa request bekler
/// 
/// Avantajları:
/// - Lock contention'ı azaltır (sadece token alırken lock)
/// - Burst request'lere izin verir (bucket doluysa)
/// - Daha adil rate limiting
/// - Yüksek concurrency'de daha iyi performans
pub struct RateLimiter {
    // Token bucket state
    tokens: AtomicU64,           // Mevcut token sayısı
    capacity: u64,                // Bucket kapasitesi (max token sayısı)
    refill_rate: f64,             // Saniyede eklenen token sayısı
    last_refill: Mutex<Instant>,  // Son token ekleme zamanı
}

impl RateLimiter {
    /// Yeni rate limiter oluşturur
    /// 
    /// min_interval_ms: Minimum request aralığı (milisaniye)
    /// Bu değer, maksimum request rate'ini belirler: 1000 / min_interval_ms req/s
    /// 
    /// Örnek: min_interval_ms = 100 → 10 req/s
    pub fn new(min_interval_ms: u64) -> Self {
        // Request rate'i hesapla (req/s)
        let requests_per_second = 1000.0 / min_interval_ms as f64;
        
        // Token bucket parametreleri
        // Capacity: Burst için izin verilen max request sayısı (örnek: 2x refill rate)
        let capacity = (requests_per_second * 2.0) as u64;
        // Refill rate: Saniyede eklenen token sayısı
        let refill_rate = requests_per_second;
        
        RateLimiter {
            tokens: AtomicU64::new(capacity), // Başlangıçta bucket dolu
            capacity,
            refill_rate,
            last_refill: Mutex::new(Instant::now()),
        }
    }

    /// Token bucket'a token ekle (refill)
    /// 
    /// Bu fonksiyon lock kullanır ama çok kısa süreli (sadece time check)
    async fn refill_tokens(&self) -> u64 {
        let mut last_refill = self.last_refill.lock().await;
        let now = Instant::now();
        let elapsed = now.duration_since(*last_refill);
        
        if elapsed.as_secs_f64() > 0.0 {
            // Geçen süreye göre token ekle
            let tokens_to_add = (elapsed.as_secs_f64() * self.refill_rate) as u64;
            
            if tokens_to_add > 0 {
                // Mevcut token sayısını al ve güncelle
                let current = self.tokens.load(Ordering::Relaxed);
                let new_tokens = (current + tokens_to_add).min(self.capacity);
                self.tokens.store(new_tokens, Ordering::Relaxed);
                
                *last_refill = now;
                
                drop(last_refill);
                return new_tokens;
            }
        }
        
        self.tokens.load(Ordering::Relaxed)
    }

    /// Bir token al (request için)
    /// 
    /// Token yoksa, token gelene kadar bekler
    pub async fn acquire_token(&self) {
        loop {
            // Önce token'ları refill et
            let available_tokens = self.refill_tokens().await;
            
            if available_tokens > 0 {
                // Token var, bir tane al
                let previous = self.tokens.fetch_sub(1, Ordering::Relaxed);
                if previous > 0 {
                    // Token başarıyla alındı
                    return;
                } else {
                    // Race condition: başka bir thread token'ı aldı
                    // Token'ı geri ekle ve tekrar dene
                    self.tokens.fetch_add(1, Ordering::Relaxed);
                }
            }
            
            // Token yok, kısa bir süre bekle ve tekrar dene
            // Refill rate'e göre bekleme süresini hesapla
            let wait_time_ms = (1000.0 / self.refill_rate) as u64;
            tokio::time::sleep(Duration::from_millis(wait_time_ms.max(1))).await;
        }
    }

    /// Bir sonraki request için bekleme süresini kontrol et ve gerekirse bekle
    /// 
    /// Backward compatibility için eski API'yi koruyoruz
    pub async fn wait_if_needed(&self) {
        self.acquire_token().await;
    }

    /// Rate limit kontrolü yapmadan sadece zamanı güncelle
    /// 
    /// Backward compatibility için eski API'yi koruyoruz
    /// Token bucket'ta bu fonksiyon gerekli değil, ama API uyumluluğu için bırakıyoruz
    pub async fn record_request(&self) {
        // Token bucket'ta token zaten acquire_token() ile alınıyor
        // Bu fonksiyon artık gerekli değil, ama API uyumluluğu için boş bırakıyoruz
    }
}

