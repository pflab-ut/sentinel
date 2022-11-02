require 'json'

file_path = '/home/test.json'
file = File.read(file_path)
data = JSON.parse(file)
puts "old data: #{data}"
data['another_key'] = 3
new_data_s = JSON.dump(data)
new_data = JSON.parse(new_data_s)
puts "new data: #{new_data}"
