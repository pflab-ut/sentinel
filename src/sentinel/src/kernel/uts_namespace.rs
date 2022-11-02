#[derive(Debug)]
pub struct UtsNameSpace {
    host_name: String,
    domain_name: String,
}

impl UtsNameSpace {
    pub fn new(host_name: String, domain_name: String) -> Self {
        Self {
            host_name,
            domain_name,
        }
    }

    pub fn host_name(&self) -> &String {
        &self.host_name
    }

    pub fn domain_name(&self) -> &String {
        &self.domain_name
    }
}
