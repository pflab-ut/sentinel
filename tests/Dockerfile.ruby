FROM ruby:3.1.1
COPY ./app/ruby/* /home/
WORKDIR /home
RUN gem install bundler
RUN bundle install
EXPOSE 4567
CMD ["bash"]
