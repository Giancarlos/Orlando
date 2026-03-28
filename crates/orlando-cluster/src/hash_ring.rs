use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash, Hasher};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SiloAddress {
    pub host: String,
    pub port: u16,
    pub silo_id: String,
}

impl SiloAddress {
    pub fn endpoint(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

pub struct HashRing {
    ring: BTreeMap<u64, SiloAddress>,
    virtual_nodes: u32,
}

impl HashRing {
    pub fn new(virtual_nodes: u32) -> Self {
        Self {
            ring: BTreeMap::new(),
            virtual_nodes,
        }
    }

    pub fn add(&mut self, silo: SiloAddress) {
        for i in 0..self.virtual_nodes {
            let key = hash_key(&format!("{}:{}", silo.silo_id, i));
            self.ring.insert(key, silo.clone());
        }
    }

    pub fn remove(&mut self, silo: &SiloAddress) {
        for i in 0..self.virtual_nodes {
            let key = hash_key(&format!("{}:{}", silo.silo_id, i));
            self.ring.remove(&key);
        }
    }

    pub fn get(&self, grain_key: &str) -> Option<&SiloAddress> {
        if self.ring.is_empty() {
            return None;
        }
        let key = hash_key(grain_key);
        self.ring
            .range(key..)
            .next()
            .or_else(|| self.ring.iter().next())
            .map(|(_, v)| v)
    }

    pub fn members(&self) -> Vec<SiloAddress> {
        let mut seen = Vec::new();
        for silo in self.ring.values() {
            if !seen.contains(silo) {
                seen.push(silo.clone());
            }
        }
        seen
    }

    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }
}

fn hash_key(key: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consistent_placement() {
        let mut ring = HashRing::new(150);
        ring.add(SiloAddress {
            host: "127.0.0.1".into(),
            port: 5001,
            silo_id: "silo-a".into(),
        });
        ring.add(SiloAddress {
            host: "127.0.0.1".into(),
            port: 5002,
            silo_id: "silo-b".into(),
        });

        // Same key always maps to same silo
        let first = ring.get("my-grain/key-1").unwrap().silo_id.clone();
        let second = ring.get("my-grain/key-1").unwrap().silo_id.clone();
        assert_eq!(first, second);
    }

    #[test]
    fn distributes_across_silos() {
        let mut ring = HashRing::new(150);
        ring.add(SiloAddress {
            host: "127.0.0.1".into(),
            port: 5001,
            silo_id: "silo-a".into(),
        });
        ring.add(SiloAddress {
            host: "127.0.0.1".into(),
            port: 5002,
            silo_id: "silo-b".into(),
        });

        let mut a_count = 0;
        let mut b_count = 0;
        for i in 0..100 {
            let target = ring.get(&format!("grain/{}", i)).unwrap();
            if target.silo_id == "silo-a" {
                a_count += 1;
            } else {
                b_count += 1;
            }
        }

        // Both silos should get some grains (not all on one)
        assert!(a_count > 10, "silo-a got {a_count} grains, expected > 10");
        assert!(b_count > 10, "silo-b got {b_count} grains, expected > 10");
    }

    #[test]
    fn remove_silo() {
        let mut ring = HashRing::new(150);
        let silo_a = SiloAddress {
            host: "127.0.0.1".into(),
            port: 5001,
            silo_id: "silo-a".into(),
        };
        let silo_b = SiloAddress {
            host: "127.0.0.1".into(),
            port: 5002,
            silo_id: "silo-b".into(),
        };
        ring.add(silo_a.clone());
        ring.add(silo_b);

        ring.remove(&silo_a);

        // All grains now go to silo-b
        for i in 0..20 {
            let target = ring.get(&format!("grain/{}", i)).unwrap();
            assert_eq!(target.silo_id, "silo-b");
        }
    }
}
