use tokio::time::{sleep, Duration, Instant};
use tokio::sync::Mutex;
use std::sync::Arc;

/// Basit Rate Limiter - RPC çağrılarını sınırlandırır
/// 
/// ✅ OPTİMİZE EDİLDİ: Token bucket algoritması yerine basit time-based rate limiting
/// 
/// Avantajları:
/// - Çok daha basit ve hafif (token bucket'a göre)
/// - Lock contention'ı minimize eder (sadece last_request güncellemesi)
/// - Yüksek concurrency'de daha iyi performans
/// - RPC rate limiting için yeterli (burst gerekmez)
/// 
/// Nasıl çalışır:
/// - Her request'ten önce son request zamanını kontrol eder
/// - Minimum interval geçmediyse, kalan süreyi bekler
/// - Son request zamanını günceller
pub struct RateLimiter {
    last_request: Arc<Mutex<Instant>>,
    min_interval: Duration,
}

impl RateLimiter {
    /// Yeni rate limiter oluşturur
    /// 
    /// min_interval_ms: Minimum request aralığı (milisaniye)
    /// Bu değer, maksimum request rate'ini belirler: 1000 / min_interval_ms req/s
    /// 
    /// Örnek: min_interval_ms = 100 → 10 req/s
    pub fn new(min_interval_ms: u64) -> Self {
        RateLimiter {
            last_request: Arc::new(Mutex::new(Instant::now())),
            min_interval: Duration::from_millis(min_interval_ms),
        }
    }

    /// Bir sonraki request için bekleme süresini kontrol et ve gerekirse bekle
    /// 
    /// ✅ OPTİMİZE EDİLDİ: Basit time-based rate limiting
    /// - Token bucket algoritması kaldırıldı (gereksiz karmaşık)
    /// - Sadece last_request zamanını kontrol eder ve gerekirse bekler
    /// - Lock sadece kısa süreli (sadece time check ve update)
    pub async fn wait_if_needed(&self) {
        let mut last = self.last_request.lock().await;
        let now = Instant::now();
        let elapsed = last.elapsed();
        
        if elapsed < self.min_interval {
            // Minimum interval geçmedi, kalan süreyi bekle
            let remaining = self.min_interval - elapsed;
            drop(last); // Lock'u bırak (sleep sırasında lock tutmuyoruz)
            sleep(remaining).await;
            // Sleep sonrası tekrar lock al ve zamanı güncelle
            let mut last = self.last_request.lock().await;
            *last = Instant::now();
        } else {
            // Minimum interval geçti, direkt devam et
            *last = now;
        }
    }

    /// Rate limit kontrolü yapmadan sadece zamanı güncelle
    /// 
    /// Backward compatibility için eski API'yi koruyoruz
    pub async fn record_request(&self) {
        let mut last = self.last_request.lock().await;
        *last = Instant::now();
    }
}

