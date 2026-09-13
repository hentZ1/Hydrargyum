use ringbuf::{HeapRb, SharedRb, storage::Heap, traits::*, wrap::caching::Caching};
use std::sync::Arc;

pub struct AudioProducer {
    pub inner: Caching<Arc<SharedRb<Heap<f32>>>, true, false>,
}

impl AudioProducer {
    fn write(&mut self, samples: &[f32]) -> usize {
        self.inner.push_slice(samples)
    }
}
pub struct AudioConsumer {
    pub inner: Caching<Arc<SharedRb<Heap<f32>>>, false, true>,
}

impl AudioConsumer {
    fn read(&mut self, destination: &mut [f32]) -> usize {
        self.inner.pop_slice(destination)
    }
}
pub fn ring_buffer(buffer_size: usize) -> (AudioProducer, AudioConsumer) {
    let rb = HeapRb::<f32>::new(buffer_size);
    let (prod, cons) = rb.split();

    let audio_producer = AudioProducer { inner: prod };
    let audio_consumer = AudioConsumer { inner: cons };

    (audio_producer, audio_consumer)
}
